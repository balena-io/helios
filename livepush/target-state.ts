import { runSshCommand } from './utils';
import type {
	Service as CompositionService,
	Volume as CompositionVolume,
	Network as CompositionNetwork,
} from '@balena/compose/dist/parse';
import type { LocalImage } from '@balena/compose/dist/multibuild';

export interface TargetService {
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

export interface SupervisorTargetRelease {
	id?: number;
	services?: { [serviceName: string]: TargetService };
	volumes?: unknown;
	networks?: unknown;
}

export interface SupervisorTarget {
	releases?: { [releaseId: string]: SupervisorTargetRelease };
	[key: string]: unknown;
}

export async function createTargetStateOverride(
	supervisorTarget: SupervisorTarget,
	servicesFromComposition: Record<string, CompositionService>,
	imagesByServiceName: { [serviceName: string]: LocalImage },
	localRelease: string,
	volumes: Record<string, CompositionVolume>,
	networks: Record<string, CompositionNetwork>,
): Promise<SupervisorTarget> {
	const [supervisorTargetRelease] = Object.values(
		supervisorTarget.releases ?? {},
	);

	const supervisorTargetServices: { [svcName: string]: TargetService } =
		supervisorTargetRelease?.services ?? {};

	let idx = 1;
	for (const [serviceName, service] of Object.entries(
		servicesFromComposition,
	)) {
		const tgtSvc: Partial<TargetService> =
			supervisorTargetServices[serviceName] ?? {};

		const { Id: dockerImageId } = await imagesByServiceName[serviceName]
			.getImage()
			.inspect();
		supervisorTargetServices[serviceName] = {
			id: idx,
			image_id: idx,
			...tgtSvc,
			image: imagesByServiceName[serviceName].name ?? service.image!,
			environment: { ...tgtSvc.environment, ...service.environment },
			labels: {
				...tgtSvc.labels,
				...service.labels,
				// Add the docker image as a label to ensure the container gets
				// recreated since the image tag won't change between push
				'io.balena.private.docker-image-id': dockerImageId,
			},
			composition: service,
		};
		idx++;
	}

	return {
		...supervisorTarget,
		releases: {
			[localRelease]: {
				...supervisorTargetRelease,
				id: 1,
				services: supervisorTargetServices,
				volumes,
				networks,
			},
		},
	};
}

export async function writeTargetStateOverride(
	remoteAddr: string,
	supervisorAppUuid: string,
	override: SupervisorTarget,
): Promise<void> {
	const overrideDir = `/tmp/balena-supervisor/apps/${supervisorAppUuid}`;
	await runSshCommand(remoteAddr, `mkdir -p ${overrideDir}`);
	await runSshCommand(
		remoteAddr,
		`echo '${JSON.stringify(override).replaceAll('"', '\\"')}' > ${overrideDir}/override.json`,
	);
}
