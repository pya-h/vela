const vscode = require('vscode');
const path = require('path');
const fs = require('fs');
const os = require('os');
const cp = require('child_process');

const TOKEN_TYPES = [
    'keyword',
    'type',
    'function',
    'method',
    'parameter',
    'variable',
    'property',
    'number',
    'string',
    'comment',
    'operator',
    'namespace',
    'enumMember'
];

const TOKEN_MODIFIERS = ['declaration', 'readonly', 'defaultLibrary'];

let client = null;
let fallbackServer = null;

function isVelaDriverBinary(binPath) {
    const base = path.basename(binPath).toLowerCase();
    return base === 'vela' || base === 'vela.exe';
}

function findInPath(binName) {
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

function resolveServerExecutable() {
    const config = vscode.workspace.getConfiguration('vela');
    const custom = config.get('serverPath');
    if (custom && custom.trim().length > 0) {
        const trimmed = custom.trim();
        return isVelaDriverBinary(trimmed)
            ? { command: trimmed, args: ['lsp'] }
            : { command: trimmed, args: [] };
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

function resolveCompilerExecutable() {
    const config = vscode.workspace.getConfiguration('vela');
    const custom = config.get('serverPath');
    if (custom && custom.trim().length > 0) {
        const trimmed = custom.trim();
        if (isVelaDriverBinary(trimmed)) {
            return trimmed;
        }
        const sibling = path.join(path.dirname(trimmed), process.platform === 'win32' ? 'vela.exe' : 'vela');
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

class StdioLspClient {
    constructor(command, args) {
        this.command = command;
        this.args = args;
        this.proc = null;
        this.ready = false;
        this.requestId = 1;
        this.pending = new Map();
        this.buffer = Buffer.alloc(0);
        this.diagnosticCollection = vscode.languages.createDiagnosticCollection('vela');
        this.onDidChangeSemanticTokensEmitter = new vscode.EventEmitter();
        this.onDidChangeSemanticTokens = this.onDidChangeSemanticTokensEmitter.event;
    }

    async start() {
        this.ready = false;
        try {
            this.proc = cp.spawn(this.command, this.args, {
                stdio: ['pipe', 'pipe', 'inherit']
            });
        } catch (err) {
            vscode.window.showErrorMessage(
                `Failed to launch Vela LSP (${this.command}): ${err.message}`
            );
            return;
        }

        this.proc.on('error', (err) => {
            this.ready = false;
            vscode.window.showErrorMessage(
                `Vela LSP error (${this.command}): ${err.message}. Install via 'cargo install --path ide/lsp'.`
            );
        });

        this.proc.on('exit', () => {
            this.ready = false;
            for (const { reject } of this.pending.values()) {
                reject(new Error('LSP server exited'));
            }
            this.pending.clear();
        });

        this.proc.stdout.on('data', (chunk) => this.handleData(chunk));

        try {
            await this.sendRequest('initialize', {
                rootUri: vscode.workspace.workspaceFolders?.[0]?.uri.toString() || null,
                capabilities: {}
            });
            this.ready = true;
            this.sendNotification('initialized', {});
            this.syncOpenDocuments();
            this.onDidChangeSemanticTokensEmitter.fire();
        } catch (err) {
            vscode.window.showErrorMessage(
                `Vela LSP initialization failed: ${err.message || err}`
            );
        }
    }

    stop() {
        this.ready = false;
        if (this.proc) {
            this.sendNotification('exit', {});
            this.proc.kill();
            this.proc = null;
        }
        for (const { reject } of this.pending.values()) {
            reject(new Error('LSP client stopped'));
        }
        this.pending.clear();
        this.diagnosticCollection.clear();
    }

    syncDocument(doc) {
        if (!this.ready || doc.languageId !== 'vela') return;
        this.sendNotification('textDocument/didOpen', {
            textDocument: {
                uri: doc.uri.toString(),
                languageId: 'vela',
                version: doc.version,
                text: doc.getText()
            }
        });
    }

    syncOpenDocuments() {
        vscode.workspace.textDocuments.forEach((doc) => this.syncDocument(doc));
    }

    sendRequest(method, params) {
        return new Promise((resolve, reject) => {
            if (!this.proc || !this.proc.stdin.writable) {
                return reject(new Error('LSP process not running'));
            }
            const id = this.requestId++;
            this.pending.set(id, { resolve, reject });
            this.send({ jsonrpc: '2.0', id, method, params });
        });
    }

    sendNotification(method, params) {
        if (!this.proc || !this.proc.stdin.writable) return;
        this.send({ jsonrpc: '2.0', method, params });
    }

    send(msg) {
        const body = Buffer.from(JSON.stringify(msg), 'utf8');
        const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'utf8');
        this.proc.stdin.write(Buffer.concat([header, body]));
    }

    handleData(chunk) {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        while (true) {
            const headerEnd = this.buffer.indexOf('\r\n\r\n');
            if (headerEnd === -1) break;

            const headerStr = this.buffer.slice(0, headerEnd).toString('utf8');
            const match = headerStr.match(/Content-Length:\s*(\d+)/i);
            if (!match) {
                this.buffer = this.buffer.slice(headerEnd + 4);
                continue;
            }

            const contentLength = parseInt(match[1], 10);
            const bodyStart = headerEnd + 4;
            const bodyEnd = bodyStart + contentLength;
            if (this.buffer.length < bodyEnd) break;

            const bodyBuf = this.buffer.slice(bodyStart, bodyEnd);
            this.buffer = this.buffer.slice(bodyEnd);

            try {
                const message = JSON.parse(bodyBuf.toString('utf8'));
                this.dispatch(message);
            } catch (err) {
                console.error('Invalid JSON from LSP:', err);
            }
        }
    }

    dispatch(message) {
        if (message.id !== undefined && this.pending.has(message.id)) {
            const { resolve, reject } = this.pending.get(message.id);
            this.pending.delete(message.id);
            if (message.error) {
                reject(message.error);
            } else {
                resolve(message.result);
            }
        } else if (message.method === 'textDocument/publishDiagnostics') {
            this.handleDiagnostics(message.params);
        }
    }

    handleDiagnostics(params) {
        if (!params || !params.uri) return;
        const uri = vscode.Uri.parse(params.uri);
        const diags = (params.diagnostics || []).map((d) => {
            const range = new vscode.Range(
                d.range.start.line,
                d.range.start.character,
                d.range.end.line,
                d.range.end.character
            );
            const severity = d.severity === 2
                ? vscode.DiagnosticSeverity.Warning
                : vscode.DiagnosticSeverity.Error;
            const diag = new vscode.Diagnostic(range, d.message, severity);
            diag.source = 'vela';
            return diag;
        });
        this.diagnosticCollection.set(uri, diags);
    }
}

async function activate(context) {
    const { command, args } = resolveServerExecutable();

    let useLanguageClient = false;
    try {
        const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
        const serverOptions = {
            run: { command, args, transport: TransportKind.stdio },
            debug: { command, args, transport: TransportKind.stdio }
        };
        const clientOptions = {
            documentSelector: [{ scheme: 'file', language: 'vela' }],
            synchronize: {
                fileEvents: vscode.workspace.createFileSystemWatcher('**/*.vela')
            }
        };
        client = new LanguageClient('vela', 'Vela Language Server', serverOptions, clientOptions);
        await client.start();
        useLanguageClient = true;
    } catch (e) {
        useLanguageClient = false;
    }

    if (!useLanguageClient) {
        fallbackServer = new StdioLspClient(command, args);
        await fallbackServer.start();

        context.subscriptions.push(
            vscode.workspace.onDidOpenTextDocument((doc) => fallbackServer.syncDocument(doc))
        );
        context.subscriptions.push(
            vscode.workspace.onDidChangeTextDocument((e) => {
                if (!fallbackServer.ready || e.document.languageId !== 'vela') return;
                fallbackServer.sendNotification('textDocument/didChange', {
                    textDocument: {
                        uri: e.document.uri.toString(),
                        version: e.document.version
                    },
                    contentChanges: [{ text: e.document.getText() }]
                });
                fallbackServer.onDidChangeSemanticTokensEmitter.fire();
            })
        );
        context.subscriptions.push(
            vscode.workspace.onDidSaveTextDocument((doc) => {
                if (!fallbackServer.ready || doc.languageId !== 'vela') return;
                fallbackServer.sendNotification('textDocument/didSave', {
                    textDocument: { uri: doc.uri.toString() }
                });
            })
        );
        context.subscriptions.push(
            vscode.workspace.onDidCloseTextDocument((doc) => {
                if (!fallbackServer.ready || doc.languageId !== 'vela') return;
                fallbackServer.sendNotification('textDocument/didClose', {
                    textDocument: { uri: doc.uri.toString() }
                });
            })
        );

        context.subscriptions.push(
            vscode.languages.registerCompletionItemProvider('vela', {
                async provideCompletionItems(doc, pos) {
                    if (!fallbackServer.ready) return [];
                    try {
                        const items = await fallbackServer.sendRequest('textDocument/completion', {
                            textDocument: { uri: doc.uri.toString() },
                            position: { line: pos.line, character: pos.character }
                        });
                        return (items || []).map((it) => {
                            // LSP CompletionItemKind is 1-based; VS Code's JS enum is 0-based.
                            const vsKind = typeof it.kind === 'number' && it.kind > 0 ? it.kind - 1 : undefined;
                            const item = new vscode.CompletionItem(it.label, vsKind);
                            if (it.detail) item.detail = it.detail;
                            if (it.filterText) item.filterText = it.filterText;
                            if (it.textEdit && it.textEdit.range) {
                                item.range = new vscode.Range(
                                    it.textEdit.range.start.line,
                                    it.textEdit.range.start.character,
                                    it.textEdit.range.end.line,
                                    it.textEdit.range.end.character
                                );
                                item.insertText = it.insertTextFormat === 2
                                    ? new vscode.SnippetString(it.textEdit.newText)
                                    : it.textEdit.newText;
                            } else if (it.insertText) {
                                item.insertText = it.insertTextFormat === 2
                                    ? new vscode.SnippetString(it.insertText)
                                    : it.insertText;
                            }
                            return item;
                        });
                    } catch (_) {
                        return [];
                    }
                }
            }, '.', '@', '<', '>', ':', '/')
        );

        context.subscriptions.push(
            vscode.languages.registerHoverProvider('vela', {
                async provideHover(doc, pos) {
                    if (!fallbackServer.ready) return null;
                    try {
                        const res = await fallbackServer.sendRequest('textDocument/hover', {
                            textDocument: { uri: doc.uri.toString() },
                            position: { line: pos.line, character: pos.character }
                        });
                        if (!res || !res.contents) return null;
                        const md = new vscode.MarkdownString(res.contents.value);
                        const range = res.range
                            ? new vscode.Range(
                                res.range.start.line,
                                res.range.start.character,
                                res.range.end.line,
                                res.range.end.character
                            )
                            : undefined;
                        return new vscode.Hover(md, range);
                    } catch (_) {
                        return null;
                    }
                }
            })
        );

        context.subscriptions.push(
            vscode.languages.registerDefinitionProvider('vela', {
                async provideDefinition(doc, pos) {
                    if (!fallbackServer.ready) return null;
                    try {
                        const res = await fallbackServer.sendRequest('textDocument/definition', {
                            textDocument: { uri: doc.uri.toString() },
                            position: { line: pos.line, character: pos.character }
                        });
                        if (!res || !res.uri) return null;
                        return new vscode.Location(
                            vscode.Uri.parse(res.uri),
                            new vscode.Range(
                                res.range.start.line,
                                res.range.start.character,
                                res.range.end.line,
                                res.range.end.character
                            )
                        );
                    } catch (_) {
                        return null;
                    }
                }
            })
        );

        context.subscriptions.push(
            vscode.languages.registerDocumentSymbolProvider('vela', {
                async provideDocumentSymbols(doc) {
                    if (!fallbackServer.ready) return [];
                    try {
                        const symbols = await fallbackServer.sendRequest('textDocument/documentSymbol', {
                            textDocument: { uri: doc.uri.toString() }
                        });
                        const mapSymbol = (s) => {
                            // LSP SymbolKind is 1-based; VS Code's JS enum is 0-based.
                            const vsKind = typeof s.kind === 'number' && s.kind > 0 ? s.kind - 1 : 0;
                            const sym = new vscode.DocumentSymbol(
                                s.name,
                                s.detail || '',
                                vsKind,
                                new vscode.Range(
                                    s.range.start.line,
                                    s.range.start.character,
                                    s.range.end.line,
                                    s.range.end.character
                                ),
                                new vscode.Range(
                                    s.selectionRange.start.line,
                                    s.selectionRange.start.character,
                                    s.selectionRange.end.line,
                                    s.selectionRange.end.character
                                )
                            );
                            if (s.children) {
                                sym.children = s.children.map(mapSymbol);
                            }
                            return sym;
                        };
                        return (symbols || []).map(mapSymbol);
                    } catch (_) {
                        return [];
                    }
                }
            })
        );

        const legend = new vscode.SemanticTokensLegend(TOKEN_TYPES, TOKEN_MODIFIERS);
        context.subscriptions.push(
            vscode.languages.registerDocumentSemanticTokensProvider('vela', {
                onDidChangeSemanticTokens: fallbackServer.onDidChangeSemanticTokens,
                async provideDocumentSemanticTokens(doc) {
                    if (!fallbackServer.ready) return null;
                    try {
                        const res = await fallbackServer.sendRequest('textDocument/semanticTokens/full', {
                            textDocument: { uri: doc.uri.toString() }
                        });
                        if (!res || !Array.isArray(res.data)) return null;
                        return new vscode.SemanticTokens(new Uint32Array(res.data));
                    } catch (_) {
                        return null;
                    }
                }
            }, legend)
        );
    }

    context.subscriptions.push(
        vscode.commands.registerCommand('vela.restartServer', async () => {
            if (client) {
                await client.stop();
                await client.start();
            } else if (fallbackServer) {
                fallbackServer.stop();
                await fallbackServer.start();
            }
            vscode.window.showInformationMessage('Vela Language Server restarted.');
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

async function deactivate() {
    if (client) {
        await client.stop();
    }
    if (fallbackServer) {
        fallbackServer.stop();
    }
}

module.exports = {
    activate,
    deactivate
};
