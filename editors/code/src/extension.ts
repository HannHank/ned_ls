// Copyright (c) 2025 Johann Hanke
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Originally based on tower-lsp-boilerplate
// (https://github.com/IWANABETHATGUY/tower-lsp-boilerplate), which derives
// from Microsoft's MIT-licensed lsp-sample.

import {
	workspace,
	EventEmitter,
	ExtensionContext,
	TextDocumentChangeEvent,
} from "vscode";

import * as os from 'os';
import * as path from 'path';
import * as fs from 'fs';
import * as vscode from 'vscode';

import {
	Disposable,
	Executable,
	LanguageClient,
	LanguageClientOptions,
	ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient;


export function getServerPath(context: vscode.ExtensionContext): string {

	// Check if user configured a custom path to the server binary
	const config = vscode.workspace.getConfiguration('ned');
	const customPath = config.get<string>('serverPath');

	// Check if the custom path is set and valid
	if (customPath && customPath.trim() !== '') {
		if (!fs.existsSync(customPath)) {
			vscode.window.showWarningMessage(`The custom server path '${customPath}' does not exist. Falling back to default behavior.`);
		} else {
			return customPath;
		}
	}

	// Platform-specific VSIX builds bundle exactly one binary for the target platform
	const binName = os.platform() === 'win32' ? 'ned_ls.exe' : 'ned_ls';
	const bundledPath = context.asAbsolutePath(path.join('bin', binName));

	if (fs.existsSync(bundledPath)) {
		return bundledPath;
	}

	// Simple fallback to just use the system PATH
	console.log(`No bundled binary found. Relying on system PATH for '${binName}'.`);

	return binName;
}

export async function activate(context: ExtensionContext) {

	const serverPath = getServerPath(context);

	const command = process.env.SERVER_PATH || serverPath;

	// execution permissions for macOS/ Linux
	if (path.isAbsolute(command)) {
		try {
			fs.chmodSync(command, 0o755);
		} catch (e) {
			console.warn(`Could not set execution permissions on the binary at ${command}`);
		}
	}

	const traceOutputChannel = vscode.window.createOutputChannel("Ned Language Server Trace", { log: true });

	const run: Executable = {
		command,
		options: {
			env: {
				...process.env,
				// eslint-disable-next-line @typescript-eslint/naming-convention
				RUST_LOG: "info", // TODO: make it configurable via settings
			},
		},
	};
	const serverOptions: ServerOptions = {
		run,
		debug: run,
	};

	let clientOptions: LanguageClientOptions = {
		documentSelector: [
			{ scheme: "file", language: "OMNeT++ NED" },
			{ scheme: "file", language: "OMNeT++ INI" },

			// fallback patterns 
			{ scheme: "file", pattern: "**/*.ned" },
			{ scheme: "file", pattern: "**/*.ini" }
		],
		traceOutputChannel,
	};

	// Create the language client and start the client.
	client = new LanguageClient("ned_ls", "ned language server", serverOptions, clientOptions);
	// activateInlayHints(context);
	client.start();
}

export function deactivate(): Thenable<void> | undefined {
	if (!client) {
		return undefined;
	}
	return client.stop();
}

export function activateInlayHints(ctx: ExtensionContext) {
	const maybeUpdater = {
		hintsProvider: null as Disposable | null,
		updateHintsEventEmitter: new EventEmitter<void>(),

		async onConfigChange() {
			this.dispose();

			const event = this.updateHintsEventEmitter.event;
		},

		onDidChangeTextDocument({ contentChanges, document }: TextDocumentChangeEvent) {
			// debugger
			// this.updateHintsEventEmitter.fire();
		},

		dispose() {
			this.hintsProvider?.dispose();
			this.hintsProvider = null;
			this.updateHintsEventEmitter.dispose();
		},
	};

	workspace.onDidChangeConfiguration(maybeUpdater.onConfigChange, maybeUpdater, ctx.subscriptions);
	workspace.onDidChangeTextDocument(maybeUpdater.onDidChangeTextDocument, maybeUpdater, ctx.subscriptions);

	maybeUpdater.onConfigChange().catch(console.error);
}
