import assert from 'assert';

import type { BalenaSDK } from 'balena-sdk';
import { getSdk } from 'balena-sdk';
import type { DeviceStateV3 } from 'balena-sdk/es2017/types/device-state';

import { logger, runSshCommand } from './utils';
import type { SupervisorTarget } from './target-state';

export interface DeviceInfo {
	arch: string;
	deviceType: string;
}

export interface DeviceConfig {
	deviceType: string;
	uuid: string;
	deviceApiKey: string;
	apiEndpoint: string;
}

export async function getDeviceArch(address: string): Promise<string> {
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

export async function getDeviceConfig(address: string): Promise<DeviceConfig> {
	logger.info(`Read configurations from ${address}`);
	const config = JSON.parse(
		await runSshCommand(address, 'cat /mnt/boot/config.json').catch((e) => {
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

	return { deviceType, uuid, deviceApiKey, apiEndpoint };
}

export async function getDeviceInfo(address: string): Promise<DeviceInfo> {
	const [arch, config] = await Promise.all([
		getDeviceArch(address),
		getDeviceConfig(address),
	]);

	return { arch, deviceType: config.deviceType };
}

export async function getBalenaSdk(config: DeviceConfig): Promise<BalenaSDK> {
	const balena = getSdk({ apiUrl: config.apiEndpoint, dataDirectory: false });
	await balena.auth.loginWithToken(config.deviceApiKey);
	return balena;
}

export async function getSupervisorTarget(
	balena: BalenaSDK,
	config: DeviceConfig,
	deviceInfo: DeviceInfo,
): Promise<{
	supervisorAppUuid: string;
	supervisorTarget: SupervisorTarget;
}> {
	const targetState: DeviceStateV3 =
		(await balena.models.device.getSupervisorTargetState(
			config.uuid,
			3,
		)) as any;

	assert(targetState[config.uuid] != null);

	const [supervisorAppUuid, supervisorTarget] =
		Object.entries(targetState[config.uuid].apps).find(
			([_, app]) => (app as any).name === `${deviceInfo.arch}-supervisor`,
		) ?? [];

	assert(
		supervisorAppUuid != null && supervisorTarget != null,
		'Device is not pinned to a supervisor release',
	);

	return {
		supervisorAppUuid,
		supervisorTarget: supervisorTarget as SupervisorTarget,
	};
}
