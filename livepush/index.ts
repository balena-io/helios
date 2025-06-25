import assert from 'assert';
import { exec as execSync } from 'child_process';
import { promises as fs } from 'fs';
import * as path from 'path';
import { promisify } from 'util';
import { setTimeout } from 'timers/promises';

import debug from 'debug';
import Docker from 'dockerode';
import yaml from 'js-yaml';
import readline from 'readline';
import tar from 'tar-fs';
import { Command } from 'commander';
import chokidar from 'chokidar';

import { getSdk } from 'balena-sdk';
import { multibuild, parse as compose } from '@balena/compose';
import type * as MultiBuild from '@balena/compose/dist/multibuild';
import type { BuildConfig } from '@balena/compose/dist/parse';
import dockerIgnore from '@balena/dockerignore';
import type { DeviceStateV3 } from 'balena-sdk/es2017/types/device-state';
import { Dockerfile } from 'livepush';
import Livepush from 'livepush';

interface DeviceInfo {
	arch: string;
	deviceType: string;
}

interface StageImageIds {
	[k: string]: string[];
}

interface TargetService {
	id: number;
	image_id: number;
	image: string;
	environment: {
		[varName: string]: string;
	};
	labels: {
		[labelName: string]: string;
	};
	contract?: object;
	composition?: object;
}

// exec helper
const exec = promisify(execSync);

// Send logger.info() and logger.debug() output to stdout
const dbg = debug('theseus-dev');
dbg.log = console.info.bind(console);

const logger = {
	info: dbg,
	error: debug('theseus-dev:error'),
	warn: debug('theseus-dev:error'),
	debug: dbg.extend('debug'),
};

debug.enable(process.env.DEBUG ?? 'theseus-dev*');

// Absolutely no escaping in this function, just be careful
async function runSshCommand(address: string, command: string) {
	// TODO: Make the port configurable
	const { stdout } = await exec(
		'ssh -p 22222 -o LogLevel=ERROR ' +
			'-o StrictHostKeyChecking=no ' +
			'-o UserKnownHostsFile=/dev/null ' +
			`root@${address} ` +
			`"${command}"`,
	);

	return stdout;
}

async function getDeviceArch(address: string) {
	const uname = await runSshCommand(address, 'uname -m').catch((e) => {
		throw new Error(`Failed to detect processor architecture: ${e.message}.`);
	});

	const cpuArch = uname.trim();
	switch (cpuArch) {
		case 'aarch64':
		case 'arm64':
			return 'aarch64';
		case 'x86_64':
			return 'amd64';
		case 'armv7l':
			return 'armv7hf';
		case 'armv6l':
			return 'rpi';
		case 'i686':
		case 'i386':
			return 'i386';
		default:
			logger.debug(`Unknown architecture: '${cpuArch}'`);
			return cpuArch;
	}
}

/**
 * Read dockerignore from the basedir and return
 * an instance of an Ignore object with some sensible defaults
 * added
 */
async function allowedFromDockerIgnore(dir: string, defaults: string[] = []) {
	const ignorePath = path.join(dir, '.dockerignore');
	const ignoreEntries = await fs
		.readFile(ignorePath, 'utf8')
		.then((contents) => contents.split(/\r?\n/))
		.catch((e) => {
			if (e.code !== 'ENOENT') {
				throw new Error(`Error reading file ${ignorePath}: ${e.message}`);
			}
			return defaults;
		});

	return dockerIgnore({ ignorecase: false })
		.add(['**/.git']) // Always ignore .git directories
		.add(ignoreEntries)
		.createFilter();
}

// Source: https://github.com/balena-io/balena-cli/blob/f6d668684a6f5ea8102a964ca1942b242eaa7ae2/lib/utils/device/live.ts#L539-L547
function extractDockerArrowMessage(outputLine: string): string | undefined {
	const arrowTest = /^.*\s*-+>\s*(.+)/i;
	const match = arrowTest.exec(outputLine);
	if (match != null) {
		return match[1];
	}
}

async function writeCache(stageIds: StageImageIds, cachePath: string) {
	if (Object.values(stageIds).length > 0) {
		logger.info(`Caching successful stage build ids in '${cachePath}'`);
		logger.debug('Stage ids', stageIds);
		// Create cache dir if it doesn't exist. Ignore any errors
		await fs.mkdir(path.dirname(cachePath), { recursive: true }).catch(() => {
			/** ignore */
		});

		// Write stage ids as cache for next build. Ignore any errors
		await fs.writeFile(cachePath, JSON.stringify(stageIds)).catch((e) => {
			logger.debug(`Could not write cache: ${e.message}`);
		});
	}
}

// Source: https://github.com/balena-io/balena-cli/blob/master/src/utils/compose_ts.ts#L414
function makeImageName(projectName: string, serviceName: string) {
	const name = `${projectName}_${serviceName}`;
	return name.toLowerCase();
}

/**
 * Return true if `image` is actually a docker-compose.yml `services.service.build`
 * configuration object, rather than an "external image" (`services.service.image`).
 *
 * The `image` argument may therefore refer to either a `build` or `image` property
 * of a service in a docker-compose.yml file, which is a bit confusing but it matches
 * the `ImageDescriptor.image` property as defined by `@balena/compose/parse`.
 *
 * Note that `@balena/compose/parse` "normalizes" the docker-compose.yml file such
 * that, if `services.service.build` is a string, it is converted to a BuildConfig
 * object with the string value assigned to `services.service.build.context`:
 * https://github.com/balena-io-modules/balena-compose/blob/v0.1.0/lib/parse/compose.ts#L166-L167
 * This is why this implementation works when `services.service.build` is defined
 * as a string in the docker-compose.yml file.
 *
 * Source: https://github.com/balena-io/balena-cli/blob/master/src/utils/compose_ts.ts#L736
 *
 * @param image The `ImageDescriptor.image` attribute parsed with `@balena/compose/parse`
 */
export function isBuildConfig(
	image: string | BuildConfig,
): image is BuildConfig {
	return image != null && typeof image !== 'string';
}

async function resolveTasks(buildTasks: MultiBuild.BuildTask[]) {
	const { cloneTarStream } = await import('tar-utils');
	// Do one task at a time in order to reduce peak memory usage. Resolves to buildTasks.
	for (const buildTask of buildTasks) {
		// buildStream is falsy for "external" tasks (image pull)
		if (!buildTask.buildStream) {
			continue;
		}
		let error: unknown;
		try {
			// Consume each task.buildStream in order to trigger the
			// resolution events that define fields like:
			//     task.dockerfile, task.dockerfilePath,
			//     task.projectType, task.resolved
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

async function performResolution(
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

async function findServiceContainers(
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
						// The container was started
						(c.State === 'running' || c.State === 'exited'),
				)
				.map((c) => [svcName, c.Id]),
		),
	);
}

type LivepushExecutor = (changed?: string, deleted?: string) => Promise<void>;

async function getLivepushExecutor(
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

async function startLivePush(remoteAddr: string) {
	logger.info('Setting up');
	const basedir = process.cwd();

	logger.info(`Read configurations from ${remoteAddr}`);
	const config = JSON.parse(
		await runSshCommand(remoteAddr, 'cat /mnt/boot/config.json').catch((e) => {
			throw new Error(
				`Failed to read config.json, this may not be a Balena device`,
				{ cause: e },
			);
		}),
	);

	const { deviceType, uuid, deviceApiKey, apiEndpoint } = config;
	assert(deviceType != null, 'Unknown device type');
	assert(
		uuid != null && deviceApiKey != null && apiEndpoint != null,
		'Balena device is not connected to API: ' +
			JSON.stringify({
				uuid,
				deviceApiKey,
				apiEndpoint,
			}),
	);

	// Get device information
	const arch = await getDeviceArch(remoteAddr);
	const deviceInfo = { arch, deviceType };

	// Login to the API
	const balena = getSdk({ apiUrl: apiEndpoint, dataDirectory: false });
	await balena.auth.loginWithToken(deviceApiKey);

	const targetState: DeviceStateV3 =
		// without the `as any` here, the method returns DeviceState instead of DeviceStateV3
		(await balena.models.device.getSupervisorTargetState(uuid, 3)) as any;

	assert(targetState[uuid] != null);

	const [supervisorAppUuid, supervisorTarget] =
		Object.entries(targetState[uuid].apps).find(
			// the DeviceTypeV3 SDK type is missing the app name
			([_, app]) => (app as any).name === `${arch}-supervisor`,
		) ?? [];

	// XXX: we also might want to check that the device is already running theseus
	// otherwise the override won't have any effect
	assert(
		supervisorAppUuid != null && supervisorTarget != null,
		'Device is not pinned to a supervisor release',
	);

	// App configurations
	const projectName = path.basename(basedir);
	const localRelease = `10ca12e1ea5e`;

	// Get a new docker instance
	const docker = new Docker({ host: remoteAddr, port: '2375' });

	// Create the tar archive
	const allowed = await allowedFromDockerIgnore(basedir);
	const tarStream = tar.pack(basedir, {
		ignore: (name) => !allowed(name),
	});

	// Load and parse the compose file
	const composeFile = yaml.load(
		await fs.readFile(path.join(basedir, 'docker-compose.yml'), {
			encoding: 'utf8',
		}),
	);
	const comp = compose.normalize(composeFile);
	const imageDescriptors = compose.parse(comp);

	// Try to load cache. Ignore errors
	const cachePath = path.join(basedir, '.cache', 'livepush.json');
	const cachedIds: StageImageIds = await fs
		.readFile(cachePath, 'utf8')
		.then((c) => JSON.parse(c))
		.catch((e) => {
			logger.debug(`Failed to read cache: ${e.message}`);
			return {};
		});

	// Create build tasks
	const buildTasks = await multibuild.splitBuildStream(comp, tarStream);

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
		// For LivePush
		(content) => new Dockerfile(content).generateLiveDockerfile(),
	);

	const stageImages: StageImageIds = {};
	const imageDescriptorsByServiceName = Object.fromEntries(
		imageDescriptors.map((desc) => [desc.serviceName, desc]),
	);
	logger.debug('Found project types:');

	// Configure tasks
	buildTasks.forEach((task) => {
		const d = imageDescriptorsByServiceName[task.serviceName];

		// multibuild (splitBuildStream) parses the composition internally so
		// any tags we've set before are lost; re-assign them here
		task.tag ??= makeImageName(projectName, task.serviceName);
		if (isBuildConfig(d.image)) {
			d.image.tag = task.tag;
		}

		task.dockerOpts ??= {};
		task.dockerOpts = {
			...task.dockerOpts,
			cachefrom: JSON.stringify([
				task.tag,
				...(task.dockerOpts?.cachefrom ?? []),
				...(cachedIds[task.serviceName] ?? []),
			]),
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

				// Parse the build output to get stage ids and
				// for logging
				let lastArrowMessage: string | undefined;
				readline.createInterface({ input: stream }).on('line', (line) => {
					// If this was a FROM line, take the last found
					// image id and save it as a stage id
					// Source: https://github.com/balena-io/balena-cli/blob/f6d668684a6f5ea8102a964ca1942b242eaa7ae2/lib/utils/device/live.ts#L300-L325
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

					// Log the build line
					logger.info(line);
				});

				return stream;
			};
		}
	});

	const images = await multibuild.performBuilds(
		buildTasks,
		docker,
		'/var/lib/docker/tmp',
	);

	const dockerImages = await Promise.all(
		images.map(async (img) => ({
			serviceName: img.serviceName,
			dockerImage: await img.getImage().inspect(),
		})),
	);
	const imagesByServiceName = Object.fromEntries(
		dockerImages.map(({ serviceName, dockerImage }) => [
			serviceName,
			dockerImage,
		]),
	);

	// Write the cache
	await writeCache(stageImages, cachePath);

	// Create target state
	logger.debug('Creating target override');
	const [supervisorTargetRelease] = Object.values(
		supervisorTarget.releases ?? {},
	);
	const { volumes, networks, services: servicesFromComposition } = comp;

	const supervisorTargetServices: { [svcName: string]: TargetService } =
		(supervisorTargetRelease?.services as any) ?? {};

	// Override any services in the target with the service from the local composition
	let idx = 1;
	for (const [serviceName, service] of Object.entries(
		servicesFromComposition,
	)) {
		const tgtSvc: Partial<TargetService> =
			supervisorTargetServices[serviceName];
		supervisorTargetServices[serviceName] = {
			/// Use fake ids if the service doesn't exist
			id: idx,
			image_id: idx,
			// The ids will be overriden by this spread if tgtSvc exists
			...tgtSvc,
			// Override key properties
			image: imagesByServiceName[serviceName].Id ?? service.image!,
			environment: { ...tgtSvc.environment, ...service.environment },
			labels: { ...tgtSvc.labels, ...service.labels },
			composition: service,
		};
		idx++;
	}

	const supervisorTargetAppOverride = {
		...supervisorTarget,
		releases: {
			[localRelease]: {
				...supervisorTargetRelease,
				id: 1, // The release ID is irrelevant
				services: supervisorTargetServices,
				volumes,
				networks,
			},
		},
	};

	// Write the target in the `/tmp/balena-supervisor` directory
	const overrideDir = `/tmp/balena-supervisor/apps/${supervisorAppUuid}`;
	await runSshCommand(remoteAddr, `mkdir -p ${overrideDir}`);
	await runSshCommand(
		remoteAddr,
		`echo '${JSON.stringify(supervisorTargetAppOverride).replaceAll('"', '\\"')}' > ${overrideDir}/override.json`,
	);

	// Trigger an update check on the supervisor
	logger.debug('Triggering update');

	// NOTE: this might not be possible soon with the deviceApiKey
	// as it doesn't make sense to go through the API if you have access
	// to the device. We just do this here since we don't have a key for the
	// supervisor API
	await balena.models.device.update(uuid, {});

	// wait 10s for device state to settle
	logger.info('Waiting for the state to settle ...');
	await setTimeout(5 * 1000);
	const serviceNames = Object.keys(servicesFromComposition);
	let serviceContainers = await findServiceContainers(
		docker,
		serviceNames,
		localRelease,
	);

	// We wait until there is a container created for every service in the composition
	while (Object.keys(serviceContainers).length !== serviceNames.length) {
		await setTimeout(1 * 1000);
		serviceContainers = await findServiceContainers(
			docker,
			serviceNames,
			localRelease,
		);
	}

	const cleanupOps: Array<() => Promise<void>> = [];

	// Set up file system watchers for build type tasks
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

		// Start watching the fs for changes
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

	let sigint = () => {
		/* noop */
	};

	// Wait until interrupt
	await new Promise((_, reject) => {
		sigint = () => {
			reject(new Error('User interrupt (Ctrl+C) received'));
		};
		process.on('SIGINT', sigint);
	});

	// Cleanup
	logger.info('Cleaning up. Please wait ...');
	await Promise.all(cleanupOps);
	process.removeListener('SIGINT', sigint);
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
