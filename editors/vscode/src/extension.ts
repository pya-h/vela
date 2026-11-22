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

function isVelaDriverBinary(binPath: string): boolean {
    const base = path.basename(binPath).toLowerCase();
    return base === 'vela' || base === 'vela.exe';
}

function findInPath(binName: string): string | null {
    const pathEnv = process.env.PATH || '';
    const dirs = pathEnv.split(path.delimiter).filter(Boolean);
    const exts = process.platform === 'win32' ? ['', '.exe'] : [''];
    for (const dir of dirs) {
        for (const ext of exts) {
            const candidate = path.join(dir, binName + ext);
            if (fs.existsSync(candidate)) {
                return candidate;
            }
        }
    }
    return null;
}

function resolveServerExecutable(): { command: string; args: string[] } {
    const config = vscode.workspace.getConfiguration('vela');
    const custom = config.get<string>('serverPath')?.trim();
    if (custom && custom.length > 0) {
        return isVelaDriverBinary(custom)
            ? { command: custom, args: ['lsp'] }
            : { command: custom, args: [] };
    }

    const home = os.homedir();
    const exeSuffix = process.platform === 'win32' ? '.exe' : '';
    const cargoLsp = path.join(home, '.cargo', 'bin', 'vela-lsp' + exeSuffix);
    if (fs.existsSync(cargoLsp)) {
        return { command: cargoLsp, args: [] };
    }

    const pathLsp = findInPath('vela-lsp');
    if (pathLsp) {
        return { command: pathLsp, args: [] };
    }

    const cargoVela = path.join(home, '.cargo', 'bin', 'vela' + exeSuffix);
    if (fs.existsSync(cargoVela)) {
        return { command: cargoVela, args: ['lsp'] };
    }

    const pathVela = findInPath('vela');
    if (pathVela) {
        return { command: pathVela, args: ['lsp'] };
    }

    return { command: 'vela-lsp', args: [] };
}

function resolveCompilerExecutable(): string {
    const config = vscode.workspace.getConfiguration('vela');
    const custom = config.get<string>('serverPath')?.trim();
    if (custom && custom.length > 0) {
        if (isVelaDriverBinary(custom)) {
            return custom;
        }
        const sibling = path.join(path.dirname(custom), process.platform === 'win32' ? 'vela.exe' : 'vela');
        if (fs.existsSync(sibling)) {
            return sibling;
        }
    }

    const home = os.homedir();
    const exeSuffix = process.platform === 'win32' ? '.exe' : '';
    const cargoVela = path.join(home, '.cargo', 'bin', 'vela' + exeSuffix);
    if (fs.existsSync(cargoVela)) {
        return cargoVela;
    }

    return 'vela';
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
        client = new LanguageClient('vela', 'Vela Language Server', serverOptions, clientOptions);
        await client.start();
    } catch (_) {
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

            if (editor.document.isUntitled) {
                vscode.window.showWarningMessage('Please save the Vela file before emitting C11.');
                return;
            }
            if (editor.document.isDirty) {
                await editor.document.save();
            }

            const docPath = editor.document.uri.fsPath;
            const velaBin = resolveCompilerExecutable();

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
