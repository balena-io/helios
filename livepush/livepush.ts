import assert from 'assert';
import * as path from 'path';

import chokidar from 'chokidar';
import type Docker from 'dockerode';
import { Dockerfile } from 'livepush';
import Livepush from 'livepush';
import type * as MultiBuild from '@balena/compose/dist/multibuild';

import type { StageImageIds } from './utils';
import { logger } from './utils';

type LivepushExecutor = (changed?: string, deleted?: string) => Promise<void>;

export async function getLivepushExecutor(
	livepush: Livepush,
): Promise<LivepushExecutor> {
	const { default: pDebounce } = await import('p-debounce');
	let changedFiles: string[] = [];
	let deletedFiles: string[] = [];
	const actualExecutor = pDebounce.promise(async () => {
		await livepush.performLivepush(changedFiles, deletedFiles);
		changedFiles = [];
		deletedFiles = [];
	});
	return (changed?: string, deleted?: string) => {
		if (changed) {
			changedFiles.push(changed);
		}
		if (deleted) {
			deletedFiles.push(deleted);
		}
		return actualExecutor();
	};
}

export async function setupLivepushWatchers(
	buildTasks: MultiBuild.BuildTask[],
	serviceContainers: { [serviceName: string]: string },
	stageImages: StageImageIds,
	docker: Docker,
	basedir: string,
): Promise<Array<() => Promise<void>>> {
	const cleanupOps: Array<() => Promise<void>> = [];

	for (const [serviceName, containerId] of Object.entries(serviceContainers)) {
		const buildTask = buildTasks.find((t) => t.serviceName === serviceName);
		assert(buildTask != null);

		if (buildTask.external) {
			continue;
		}

		assert(buildTask.dockerfile != null);
		assert(buildTask.context != null);

		const dockerfile = new Dockerfile(buildTask.dockerfile);
		const context = path.resolve(basedir, buildTask.context);
		const livepush = await Livepush.init({
			dockerfile,
			context,
			containerId,
			stageImages: stageImages[serviceName] ?? [],
			docker,
		});

		const buildVars = buildTask.buildMetadata.getBuildVarsForService(
			buildTask.serviceName,
		);

		if (Object.values(buildVars).length > 0) {
			livepush.setBuildArgs(buildVars);
		}

		livepush.on('commandExecute', ({ command }) => {
			logger.info(`Executing command: \`${command}\``);
		});
		livepush.on('commandOutput', ({ output }) => {
			logger.info(`  ${output.data.toString()}`);
		});
		livepush.on('commandReturn', ({ returnCode, command }) => {
			if (returnCode !== 0) {
				logger.error(
					`  Command ${command} failed with exit code: ${returnCode}`,
				);
			} else {
				logger.debug(`Command ${command} exited successfully`);
			}
		});
		livepush.on('containerRestart', () => {
			logger.info('Restarting service');
		});
		livepush.on('cancel', () => {
			logger.info('Cancelling current livepush...');
		});

		const livepushExecutor = await getLivepushExecutor(livepush);

		logger.debug('Listening for changes to ', context);
		const watcher = chokidar
			.watch(context, {
				ignored: /((^|[/\\])\..|(node_modules|target|test|livepush)\/.*)/,
				ignoreInitial: true,
			})
			.on('add', (filePath) => livepushExecutor(filePath))
			.on('change', (filePath) => livepushExecutor(filePath))
			.on('unlink', (filePath) => livepushExecutor(undefined, filePath));

		cleanupOps.push(async () => {
			await watcher.close();
			await livepush.cleanupIntermediateContainers();
		});
	}

	return cleanupOps;
}
