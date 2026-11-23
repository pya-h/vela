import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import * as cp from 'child_process';
import * as vscode from 'vscode';

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

interface LspPosition {
    line: number;
    character: number;
}

interface LspRange {
    start: LspPosition;
    end: LspPosition;
}

interface LspMessage {
    jsonrpc?: string;
    id?: number;
    method?: string;
    params?: unknown;
    result?: unknown;
    error?: { message?: string };
}

let lspClient: StdioLspClient | null = null;

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

class StdioLspClient {
    private proc: cp.ChildProcessWithoutNullStreams | null = null;
    public ready = false;
    private requestId = 1;
    private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>();
    private buffer = Buffer.alloc(0);
    private diagnosticCollection = vscode.languages.createDiagnosticCollection('vela');
    public readonly onDidChangeSemanticTokensEmitter = new vscode.EventEmitter<void>();
    public readonly onDidChangeSemanticTokens = this.onDidChangeSemanticTokensEmitter.event;

    constructor(private readonly output: vscode.OutputChannel) {}

    private trace(direction: string, message: LspMessage): void {
        const level = vscode.workspace.getConfiguration('vela').get<string>('trace.server') || 'off';
        if (level === 'off') return;
        const label = message.method || `response#${message.id}`;
        if (level === 'verbose') {
            this.output.appendLine(`[${direction}] ${label}: ${JSON.stringify(message)}`);
        } else {
            this.output.appendLine(`[${direction}] ${label}`);
        }
    }

    async start(): Promise<void> {
        const { command, args } = resolveServerExecutable();
        this.ready = false;
        try {
            this.proc = cp.spawn(command, args, {
                stdio: ['pipe', 'pipe', 'pipe']
            });
        } catch (err) {
            const msg = err instanceof Error ? err.message : String(err);
            vscode.window.showErrorMessage(`Failed to launch Vela LSP (${command}): ${msg}`);
            return;
        }

        this.proc.stderr.on('data', (chunk: Buffer) => {
            this.output.append(chunk.toString('utf8'));
        });

        this.proc.on('error', (err: Error) => {
            this.ready = false;
            vscode.window.showErrorMessage(
                `Vela LSP error (${command}): ${err.message}. Install via 'cargo install --path ide/lsp'.`
            );
        });

        this.proc.on('exit', () => {
            this.ready = false;
            for (const { reject } of this.pending.values()) {
                reject(new Error('LSP server exited'));
            }
            this.pending.clear();
        });

        this.proc.stdout.on('data', (chunk: Buffer) => this.handleData(chunk));

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
            const msg = err instanceof Error ? err.message : String(err);
            vscode.window.showErrorMessage(`Vela LSP initialization failed: ${msg}`);
        }
    }

    stop(): void {
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

    syncDocument(doc: vscode.TextDocument): void {
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

    syncOpenDocuments(): void {
        vscode.workspace.textDocuments.forEach((doc) => this.syncDocument(doc));
    }

    sendRequest<T = unknown>(method: string, params: unknown): Promise<T> {
        return new Promise((resolve, reject) => {
            if (!this.proc || !this.proc.stdin.writable) {
                reject(new Error('LSP process not running'));
                return;
            }
            const id = this.requestId++;
            this.pending.set(id, {
                resolve: (v) => resolve(v as T),
                reject
            });
            const msg: LspMessage = { jsonrpc: '2.0', id, method, params };
            this.trace('client -> server', msg);
            this.send({ jsonrpc: '2.0', id, method, params });
        });
    }

    sendNotification(method: string, params: unknown): void {
        if (!this.proc || !this.proc.stdin.writable) return;
        this.trace('client -> server', { method, params });
        this.send({ jsonrpc: '2.0', method, params });
    }

    private send(msg: Record<string, unknown>): void {
        if (!this.proc) return;
        const body = Buffer.from(JSON.stringify(msg), 'utf8');
        const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'utf8');
        this.proc.stdin.write(Buffer.concat([header, body]));
    }

    private handleData(chunk: Buffer): void {
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
                const message = JSON.parse(bodyBuf.toString('utf8')) as LspMessage;
                this.trace('server -> client', message);
                this.dispatch(message);
            } catch (err) {
                this.output.appendLine(`Invalid JSON from LSP: ${err}`);
            }
        }
    }

    private dispatch(message: LspMessage): void {
        if (message.id !== undefined && this.pending.has(message.id)) {
            const entry = this.pending.get(message.id);
            this.pending.delete(message.id);
            if (entry) {
                if (message.error) {
                    entry.reject(message.error);
                } else {
                    entry.resolve(message.result);
                }
            }
        } else if (message.method === 'textDocument/publishDiagnostics') {
            this.handleDiagnostics(message.params as { uri?: string; diagnostics?: Array<{ range: LspRange; severity?: number; message: string }> });
        }
    }

    private handleDiagnostics(params: { uri?: string; diagnostics?: Array<{ range: LspRange; severity?: number; message: string }> } | undefined): void {
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

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    const output = vscode.window.createOutputChannel('Vela Language Server');
    context.subscriptions.push(output);

    const client = new StdioLspClient(output);
    lspClient = client;
    context.subscriptions.push(client.onDidChangeSemanticTokensEmitter);
    await client.start();

    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument((doc) => client.syncDocument(doc))
    );
    context.subscriptions.push(
        vscode.workspace.onDidChangeTextDocument((e) => {
            if (!client.ready || e.document.languageId !== 'vela' || e.contentChanges.length === 0) return;
            client.sendNotification('textDocument/didChange', {
                textDocument: {
                    uri: e.document.uri.toString(),
                    version: e.document.version
                },
                contentChanges: [{ text: e.document.getText() }]
            });
            client.onDidChangeSemanticTokensEmitter.fire();
        })
    );
    context.subscriptions.push(
        vscode.workspace.onDidSaveTextDocument((doc) => {
            if (!client.ready || doc.languageId !== 'vela') return;
            client.sendNotification('textDocument/didSave', {
                textDocument: { uri: doc.uri.toString() }
            });
        })
    );
    context.subscriptions.push(
        vscode.workspace.onDidCloseTextDocument((doc) => {
            if (!client.ready || doc.languageId !== 'vela') return;
            client.sendNotification('textDocument/didClose', {
                textDocument: { uri: doc.uri.toString() }
            });
        })
    );

    context.subscriptions.push(
        vscode.languages.registerCompletionItemProvider('vela', {
            async provideCompletionItems(doc, pos) {
                if (!client.ready) return [];
                try {
                    const items = await client.sendRequest<Array<{
                        label: string;
                        kind?: number;
                        detail?: string;
                        filterText?: string;
                        insertText?: string;
                        insertTextFormat?: number;
                        textEdit?: { range: LspRange; newText: string };
                    }>>('textDocument/completion', {
                        textDocument: { uri: doc.uri.toString() },
                        position: { line: pos.line, character: pos.character }
                    });
                    return (items || []).map((it) => {
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
                if (!client.ready) return null;
                try {
                    const res = await client.sendRequest<{ contents?: { value: string }; range?: LspRange } | null>('textDocument/hover', {
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
                if (!client.ready) return null;
                try {
                    const res = await client.sendRequest<{ uri?: string; range: LspRange } | null>('textDocument/definition', {
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

    interface LspSymbol {
        name: string;
        detail?: string;
        kind: number;
        range: LspRange;
        selectionRange: LspRange;
        children?: LspSymbol[];
    }

    context.subscriptions.push(
        vscode.languages.registerDocumentSymbolProvider('vela', {
            async provideDocumentSymbols(doc) {
                if (!client.ready) return [];
                try {
                    const symbols = await client.sendRequest<LspSymbol[]>('textDocument/documentSymbol', {
                        textDocument: { uri: doc.uri.toString() }
                    });
                    const mapSymbol = (s: LspSymbol): vscode.DocumentSymbol => {
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
            onDidChangeSemanticTokens: client.onDidChangeSemanticTokens,
            async provideDocumentSemanticTokens(doc) {
                if (!client.ready) return null;
                try {
                    const res = await client.sendRequest<{ data?: number[] } | null>('textDocument/semanticTokens/full', {
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

    context.subscriptions.push(
        vscode.commands.registerCommand('vela.restartServer', async () => {
            client.stop();
            await client.start();
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

export function deactivate(): void {
    if (lspClient) {
        lspClient.stop();
    }
}
