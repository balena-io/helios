/**
 * @fileoverview Utility functions for the livepush development tool.
 * Contains common functions for SSH communication, Docker ignore handling,
 * logging, and build configuration utilities.
 */

import { exec as execSync } from 'child_process';
import { promises as fs } from 'fs';
import * as path from 'path';
import { promisify } from 'util';

import dockerIgnore from '@balena/dockerignore';
import type { BuildConfig } from '@balena/compose/dist/parse';
import debug from 'debug';

const exec = promisify(execSync);

const dbg = debug('theseus-dev');
dbg.log = console.info.bind(console);

export const logger = {
	info: dbg,
	error: debug('theseus-dev:error'),
	warn: debug('theseus-dev:error'),
	debug: dbg.extend('debug'),
};

debug.enable(process.env.DEBUG ?? 'theseus-dev*');

export interface StageImageIds {
	[k: string]: string[];
}

/**
 * Executes a command on a remote device via SSH.
 * Uses fixed SSH options for Balena devices on port 22222.
 *
 * @param address - IP address of the remote device
 * @param command - Command to execute on the remote device
 * @returns Promise resolving to the command's stdout
 */
export async function runSshCommand(
	address: string,
	command: string,
): Promise<string> {
	const { stdout } = await exec(
		'ssh -p 22222 -o LogLevel=ERROR ' +
			'-o StrictHostKeyChecking=no ' +
			'-o UserKnownHostsFile=/dev/null ' +
			`root@${address} ` +
			`"${command}"`,
	);

	return stdout;
}

/**
 * Creates a filter function based on .dockerignore rules.
 * Reads the .dockerignore file from the specified directory and creates
 * a filter that determines which files should be included in the build context.
 *
 * @param dir - Directory containing the .dockerignore file
 * @param defaults - Default ignore patterns to use if .dockerignore doesn't exist
 * @returns Function that returns true if a file should be included
 */
export async function allowedFromDockerIgnore(
	dir: string,
	defaults: string[] = [],
) {
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
		.add(['**/.git'])
		.add(ignoreEntries)
		.createFilter();
}

export function extractDockerArrowMessage(
	outputLine: string,
): string | undefined {
	const arrowTest = /^.*\s*-+>\s*(.+)/i;
	const match = arrowTest.exec(outputLine);
	if (match != null) {
		return match[1];
	}
}

export async function writeCache(
	stageIds: StageImageIds,
	cachePath: string,
): Promise<void> {
	if (Object.values(stageIds).length > 0) {
		logger.info(`Caching successful stage build ids in '${cachePath}'`);
		logger.debug('Stage ids', stageIds);
		await fs.mkdir(path.dirname(cachePath), { recursive: true }).catch(() => {
			/* ignore */
		});

		await fs.writeFile(cachePath, JSON.stringify(stageIds)).catch((e) => {
			logger.debug(`Could not write cache: ${e.message}`);
		});
	}
}

export function makeImageName(
	projectName: string,
	serviceName: string,
): string {
	const name = `${projectName}_${serviceName}`;
	return name.toLowerCase();
}

export function isBuildConfig(
	image: string | BuildConfig,
): image is BuildConfig {
	return image != null && typeof image !== 'string';
}
