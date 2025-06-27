import { promises as fs } from 'fs';
import * as path from 'path';
import { setTimeout } from 'timers/promises';

import Docker from 'dockerode';
import yaml from 'js-yaml';
import { Command } from 'commander';

import { multibuild, parse as compose } from '@balena/compose';
import { Dockerfile } from 'livepush';

import type { DeviceInfo } from './device';
import {
	getBalenaSdk,
	getDeviceConfig,
	getDeviceInfo,
	getSupervisorTarget,
} from './device';
import {
	createTarStream,
	performResolution,
	configureBuildTasks,
	findServiceContainers,
} from './docker';
import { setupLivepushWatchers } from './livepush';
import {
	createTargetStateOverride,
	writeTargetStateOverride,
} from './target-state';
import type { StageImageIds } from './utils';
import { logger, writeCache } from './utils';

/**
 * Builds and deploys services to the remote device
 */
async function buildAndDeployServices(
	basedir: string,
	deviceInfo: DeviceInfo,
	projectName: string,
	localRelease: string,
	docker: Docker,
) {
	const tarStream = await createTarStream(basedir);

	const composition = yaml.load(
		await fs.readFile(path.join(basedir, 'docker-compose.yml'), {
			encoding: 'utf8',
		}),
	);
	const normalizedComposition = compose.normalize(composition);
	const imageDescriptors = compose.parse(normalizedComposition);

	const cachePath = path.join(basedir, '.cache', 'livepush.json');
	const cachedIds: StageImageIds = await fs
		.readFile(cachePath, 'utf8')
		.then((c) => JSON.parse(c))
		.catch((e) => {
			logger.debug(`Failed to read cache: ${e.message}`);
			return {};
		});

	const buildTasks = await multibuild.splitBuildStream(
		normalizedComposition,
		tarStream,
	);

	logger.debug('Found build tasks: ');
	buildTasks.forEach((task) => {
		if (task.external) {
			logger.debug(`    ${task.serviceName}: image pull [${task.imageName}]`);
		} else {
			logger.debug(`    ${task.serviceName}: build [${task.context}]`);
		}
		task.logger = logger;
	});

	logger.debug(
		`Resolving services with [${deviceInfo.deviceType}|${deviceInfo.arch}]`,
	);

	await performResolution(
		buildTasks,
		deviceInfo,
		projectName,
		localRelease,
		(content) => new Dockerfile(content).generateLiveDockerfile(),
	);

	const stageImages: StageImageIds = {};
	const imageDescriptorsByServiceName = Object.fromEntries(
		imageDescriptors.map((desc) => [desc.serviceName, desc]),
	);

	configureBuildTasks(
		buildTasks,
		imageDescriptorsByServiceName,
		projectName,
		cachedIds,
		stageImages,
	);

	const images = await multibuild.performBuilds(
		buildTasks,
		docker,
		'/var/lib/docker/tmp',
	);

	const imagesByServiceName = Object.fromEntries(
		images.map((img) => [img.serviceName, img]),
	);

	await writeCache(stageImages, cachePath);

	return {
		composition: normalizedComposition,
		buildTasks,
		imagesByServiceName,
		stageImages,
	};
}

/**
 * Waits for containers to be created and starts livepush watchers
 */
async function waitForContainersAndSetupWatchers(
	docker: Docker,
	serviceNames: string[],
	localRelease: string,
	buildTasks: any[],
	stageImages: StageImageIds,
	basedir: string,
) {
	let serviceContainers = {};

	while (Object.keys(serviceContainers).length < serviceNames.length) {
		logger.info('Waiting for the state to settle ...');
		await setTimeout(5 * 1000);
		serviceContainers = await findServiceContainers(
			docker,
			serviceNames,
			localRelease,
		);
	}

	return await setupLivepushWatchers(
		buildTasks,
		serviceContainers,
		stageImages,
		docker,
		basedir,
	);
}

/**
 * Main entry point for the livepush development session
 */
async function startLivePush(remoteAddr: string): Promise<void> {
	logger.info('Setting up');
	const basedir = process.cwd();

	const config = await getDeviceConfig(remoteAddr);
	const deviceInfo = await getDeviceInfo(remoteAddr);
	const balena = await getBalenaSdk(config);
	const { supervisorAppUuid, supervisorTarget } = await getSupervisorTarget(
		balena,
		config,
		deviceInfo,
	);

	const projectName = path.basename(basedir);
	const localRelease = `10ca12e1ea5e`;

	const docker = new Docker({ host: remoteAddr, port: '2375' });

	const { composition, buildTasks, imagesByServiceName, stageImages } =
		await buildAndDeployServices(
			basedir,
			deviceInfo,
			projectName,
			localRelease,
			docker,
		);

	logger.debug('Creating target override');
	const supervisorTargetAppOverride = await createTargetStateOverride(
		supervisorTarget,
		composition.services,
		imagesByServiceName,
		localRelease,
		composition.volumes ?? {},
		composition.networks ?? {},
	);

	await writeTargetStateOverride(
		remoteAddr,
		supervisorAppUuid,
		supervisorTargetAppOverride,
	);

	logger.debug('Triggering update');
	// NOTE: this may not be possible soon with just the deviceApiKey
	await balena.models.device.update(config.uuid, {});

	const serviceNames = Object.keys(composition.services);
	const cleanupOps = await waitForContainersAndSetupWatchers(
		docker,
		serviceNames,
		localRelease,
		buildTasks,
		stageImages,
		basedir,
	);

	await new Promise((_, reject) => {
		process.once('SIGINT', () => {
			reject(new Error('User interrupt (Ctrl+C) received'));
		});
	});

	logger.info('Cleaning up. Please wait ...');
	await Promise.all(cleanupOps.map((cleanup) => cleanup()));
}

// Run livepush
void (async () => {
	const program = new Command();

	// Configure program options
	program
		.name('npm run dev -- ')
		.description('Run balenaSupervisor development session')
		.argument('<address>', 'IP address of the remote host')
		.parse();

	await startLivePush(program.args[0]);
})();
