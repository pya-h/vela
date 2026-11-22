import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import * as cp from 'child_process';
import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

function resolveServerExecutable(): { command: string; args: string[] } {
    const config = vscode.workspace.getConfiguration('vela');
    const custom = config.get<string>('serverPath')?.trim();
    if (custom && custom.length > 0) {
        if (custom.endsWith('vela')) {
            return { command: custom, args: ['lsp'] };
        }
        return { command: custom, args: [] };
    }

    const home = os.homedir();
    const cargoLsp = path.join(home, '.cargo', 'bin', 'vela-lsp');
    if (fs.existsSync(cargoLsp)) {
        return { command: cargoLsp, args: [] };
    }

    const cargoVela = path.join(home, '.cargo', 'bin', 'vela');
    if (fs.existsSync(cargoVela)) {
        return { command: cargoVela, args: ['lsp'] };
    }

    return { command: 'vela-lsp', args: [] };
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    const { command, args } = resolveServerExecutable();

    const serverOptions: ServerOptions = {
        run: { command, args, transport: TransportKind.stdio },
        debug: { command, args, transport: TransportKind.stdio }
    };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'vela' }],
        synchronize: {
            fileEvents: vscode.workspace.createFileSystemWatcher('**/*.vela')
        }
    };

    try {
        client = new LanguageClient('velaLsp', 'Vela Language Server', serverOptions, clientOptions);
        await client.start();
    } catch (err) {
        vscode.window.showErrorMessage(
            `Failed to start Vela Language Server (${command}). Ensure vela-lsp is installed via 'cargo install --path ide/lsp'.`
        );
    }

    context.subscriptions.push(
        vscode.commands.registerCommand('vela.restartServer', async () => {
            if (client) {
                await client.stop();
                await client.start();
                vscode.window.showInformationMessage('Vela Language Server restarted.');
            }
        })
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('vela.showEmittedC', async () => {
            const editor = vscode.window.activeTextEditor;
            if (!editor || editor.document.languageId !== 'vela') {
                vscode.window.showWarningMessage('Please open a Vela file to view emitted C11.');
                return;
            }

            const docPath = editor.document.uri.fsPath;
            const home = os.homedir();
            const velaBin = fs.existsSync(path.join(home, '.cargo', 'bin', 'vela'))
                ? path.join(home, '.cargo', 'bin', 'vela')
                : 'vela';

            cp.execFile(velaBin, [docPath, '--emit-c'], (error, stdout, stderr) => {
                if (error) {
                    vscode.window.showErrorMessage(`Failed to emit C: ${stderr || error.message}`);
                    return;
                }

                vscode.workspace.openTextDocument({ content: stdout, language: 'c' }).then((doc) => {
                    vscode.window.showTextDocument(doc, vscode.ViewColumn.Beside, true);
                });
            });
        })
    );
}

export async function deactivate(): Promise<void> {
    if (client) {
        await client.stop();
    }
}
