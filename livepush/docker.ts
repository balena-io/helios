import readline from 'readline';

import type Docker from 'dockerode';
import tar from 'tar-fs';
import { multibuild } from '@balena/compose';
import type * as MultiBuild from '@balena/compose/dist/multibuild';

import type { DeviceInfo } from './device';
import type { StageImageIds } from './utils';
import {
	logger,
	allowedFromDockerIgnore,
	extractDockerArrowMessage,
	makeImageName,
	isBuildConfig,
} from './utils';
import type { ImageDescriptor } from '@balena/compose/dist/parse';

export async function createTarStream(basedir: string) {
	const allowed = await allowedFromDockerIgnore(basedir);
	return tar.pack(basedir, {
		ignore: (name) => !allowed(name),
	});
}

export async function resolveTasks(
	buildTasks: MultiBuild.BuildTask[],
): Promise<void> {
	const { cloneTarStream } = await import('tar-utils');
	for (const buildTask of buildTasks) {
		if (!buildTask.buildStream) {
			continue;
		}
		let error: unknown;
		try {
			buildTask.buildStream = await cloneTarStream(buildTask.buildStream);
		} catch (e) {
			error = e;
		}
		if (error || (!buildTask.external && !buildTask.resolved)) {
			const cause = error ? `${error}\n` : '';
			throw new Error(
				`${cause}Project type for service "${buildTask.serviceName}" could not be determined. Missing a Dockerfile?`,
			);
		}
	}
}

export async function performResolution(
	tasks: MultiBuild.BuildTask[],
	deviceInfo: DeviceInfo,
	appName: string,
	releaseHash: string,
	preprocessHook?: (dockerfile: string) => string,
): Promise<MultiBuild.BuildTask[]> {
	const resolveListeners: MultiBuild.ResolveListeners = {};
	const resolvePromise = new Promise<never>((_resolve, reject) => {
		resolveListeners.error = [reject];
	});
	const buildTasks = multibuild.performResolution(
		tasks,
		deviceInfo.arch,
		deviceInfo.deviceType,
		resolveListeners,
		{
			BALENA_RELEASE_HASH: releaseHash,
			BALENA_APP_NAME: appName,
		},
		preprocessHook,
	);
	await Promise.race([resolvePromise, resolveTasks(buildTasks)]);
	return buildTasks;
}

export function configureBuildTasks(
	buildTasks: MultiBuild.BuildTask[],
	imageDescriptorsByServiceName: { [svcName: string]: ImageDescriptor },
	projectName: string,
	cachedIds: StageImageIds,
	stageImages: StageImageIds,
): void {
	buildTasks.forEach((task) => {
		const d = imageDescriptorsByServiceName[task.serviceName];

		task.tag ??= makeImageName(projectName, task.serviceName);
		if (isBuildConfig(d.image)) {
			d.image.tag = task.tag;
		}

		task.dockerOpts ??= {};
		task.dockerOpts = {
			...task.dockerOpts,
			cachefrom: [
				task.tag,
				...(task.dockerOpts?.cachefrom ?? []),
				...(cachedIds[task.serviceName] ?? []),
			],
			t: task.tag,
		};

		if (task.external) {
			logger.debug(`    ${task.serviceName}: External image`);
			task.progressHook = (progress) => {
				logger.info(task.serviceName + ': ' + progress);
			};
		} else {
			logger.debug(`    ${task.serviceName}: ${task.projectType}`);
			task.streamHook = (stream) => {
				logger.info(`Starting image build`);
				stageImages[task.serviceName] = [];

				let lastArrowMessage: string | undefined;
				readline.createInterface({ input: stream }).on('line', (line) => {
					if (
						/step \d+(?:\/\d+)?\s*:\s*FROM/i.test(line) &&
						lastArrowMessage != null
					) {
						stageImages[task.serviceName].push(lastArrowMessage);
					} else {
						const msg = extractDockerArrowMessage(line);
						if (msg != null) {
							lastArrowMessage = msg;
						}
					}

					logger.info(line);
				});
			};
		}
		task.logger = logger;
	});
}

export async function findServiceContainers(
	docker: Docker,
	serviceNames: string[],
	localRelease: string,
): Promise<{ [serviceName: string]: string }> {
	const containers = await docker.listContainers({ all: true });
	return Object.fromEntries(
		serviceNames.flatMap((svcName) =>
			containers
				.filter(
					(c) =>
						c.Names.some(
							(n) =>
								n.startsWith(`/${svcName}_`) && n.endsWith(`_${localRelease}`),
						) &&
						(c.State === 'running' || c.State === 'exited'),
				)
				.map((c) => [svcName, c.Id]),
		),
	);
}
