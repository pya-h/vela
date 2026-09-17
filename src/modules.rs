//! Programs made of several files. A file includes another with
//! `include geometry`, which loads `geometry.vela` from its folder, and names
//! what it declares as `geometry.area`; `include geometry (area)` also brings
//! `area` in unqualified.
//!
//! Loading reads every file a program includes, from the file it is compiled
//! from (the main file). Resolving then gives every task, shape, and kind one name
//! for the whole program: its own name in the main file, and `module.Name` in
//! any other (`geometry.Point`). Module names are unique in a program, so these
//! names never collide, and they read as Vela does. Every name written in a
//! file that stands for one of them is rewritten to it, so the checker and the
//! code generator see one program in which every name means one thing.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::Lexer;
use crate::parser::{MAX_NESTING, Parser};

/// One file of a program: file number `i` of a program is `files[i]`, and
/// positions in it have `Span::file == i`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// The path the file was read from, relative to where the main file's path
    /// is relative to.
    pub path: PathBuf,
    pub source: String,
    /// Where the `include` that loaded it is, in another file of the program;
    /// none for the main file.
    pub included_at: Option<Span>,
}

impl SourceFile {
    /// The position in the main file that leads to `span` in `files`: `span`
    /// itself in the main file, and otherwise the `include` in the main file
    /// through which its file was loaded.
    pub fn in_main_file(files: &[SourceFile], span: Span) -> Span {
        let mut at = span;
        // Each step goes to a file loaded earlier, so this ends.
        for _ in 0..files.len() {
            if at.file == 0 {
                break;
            }
            match files.get(at.file).and_then(|file| file.included_at) {
                Some(included) => at = included,
                None => break,
            }
        }
        at
    }
}

/// The file `include` path `parts` names, from a file in `folder`: the parts joined
/// with `.vela` added, and `.` and `..` settled.
pub fn module_file(folder: &Path, parts: &[String]) -> PathBuf {
    let mut path = folder.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path.set_extension("vela");
    normalize(&path)
}

/// Reads the file at a path, for loading a program's modules. The command line
/// reads from the disk; an editor can give the text of unsaved files, or keep
/// a program inside its folder.
pub type Loader<'a> = dyn FnMut(&Path) -> io::Result<String> + 'a;

/// What the checker needs to know about one module of a program.
#[derive(Debug, Clone, Default)]
pub struct ModuleScope {
    /// The module's name, the prefix of its items' names; empty for the main file.
    pub name: String,
    /// The file it is.
    pub file: usize,
    /// The names of the modules it includes, each with the position of its
    /// `include`.
    pub includes: Vec<Binding>,
    /// The names `include m (a, b)` brings in unqualified: each as written, the
    /// name it stands for (`m.a`), and the position where it is brought in.
    pub imports: Vec<(String, String, Span)>,
}

impl ModuleScope {
    /// The name that `name`, written unqualified in this module, stands for, if
    /// it is one brought in with `include`.
    pub fn imported(&self, name: &str) -> Option<&str> {
        self.imports.iter().find(|(written, _, _)| written == name).map(|(_, full, _)| full.as_str())
    }
}

/// A loaded and resolved program, ready to check.
#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Every item of every module, the main file's last.
    pub items: Vec<Item>,
    /// One per module, the main file's first.
    pub modules: Vec<ModuleScope>,
    /// Where the parts of written types are, in every file.
    pub type_places: TypePlaces,
}

/// How many modules can include one another in a chain, like `MAX_NESTING` for
/// code: loading follows each chain recursively.
const MAX_MODULE_DEPTH: usize = MAX_NESTING;

/// Loads the program whose main file is `source`, read from `path`, with every
/// module it includes, directly or through other modules, read by `loader`. The
/// files are returned even when loading fails, so an error in any of them can
/// be shown with its source line.
pub fn load(path: &Path, source: &str, loader: &mut Loader) -> (Vec<SourceFile>, Result<Program>) {
    let mut state = State::default();
    let result = state.load(path, source, loader);
    (state.files, result)
}

/// A module being loaded.
struct Module {
    /// Its name, and the prefix of its items' names; empty for the main file.
    name: String,
    file: usize,
    includes: Vec<Include>,
    /// For each of `includes`, the module it loads.
    included: Vec<usize>,
    items: Vec<Item>,
}

#[derive(Default)]
struct State {
    files: Vec<SourceFile>,
    modules: Vec<Module>,
    /// Each loaded file's module, by its path with `.` and `..` settled.
    by_path: HashMap<PathBuf, usize>,
    /// The modules being loaded, outermost first, to find a module that
    /// includes itself through others.
    loading: Vec<usize>,
    /// The modules whose own modules are all loaded, each after the modules it
    /// includes.
    order: Vec<usize>,
    type_places: TypePlaces,
}

impl State {
    fn load(&mut self, path: &Path, source: &str, loader: &mut Loader) -> Result<Program> {
        let main = self.parse(String::new(), path, source.to_string(), None)?;
        self.visit(main, loader)?;
        let tables: Vec<Names> = self.modules.iter().map(own_names).collect();
        let mut scopes = Vec::new();
        for index in 0..self.modules.len() {
            scopes.push(self.resolve(index, &tables)?);
        }
        let mut items = Vec::new();
        for index in std::mem::take(&mut self.order) {
            if let Some(module) = self.modules.get_mut(index) {
                items.append(&mut module.items);
            }
        }
        Ok(Program { items, modules: scopes, type_places: std::mem::take(&mut self.type_places) })
    }

    /// Lexes and parses `source` as the next file, the module called `name`,
    /// loaded by the `include` at `included_at`.
    fn parse(&mut self, name: String, path: &Path, source: String, included_at: Option<Span>) -> Result<usize> {
        let file = self.files.len();
        self.files.push(SourceFile { path: path.to_path_buf(), source, included_at });
        let source = self.files.get(file).map_or("", |f| f.source.as_str());
        let (tokens, spans) = Lexer::for_file(source, file).tokenize()?;
        let mut parser = Parser::new(tokens, spans);
        let items = parser.parse_file()?;
        self.type_places.extend(parser.type_places());
        let includes = parser.includes();
        let index = self.modules.len();
        self.by_path.insert(normalize(path), index);
        self.modules.push(Module { name, file, includes, included: Vec::new(), items });
        Ok(index)
    }

    /// Loads every module that module `index` includes, and theirs, in order.
    fn visit(&mut self, index: usize, loader: &mut Loader) -> Result<()> {
        if self.loading.len() >= MAX_MODULE_DEPTH {
            let span = self.modules.get(index).and_then(|m| m.includes.first()).map_or_else(Span::default, |u| u.span);
            return err(span, format!("modules include one another too deeply (more than {} in a chain)", MAX_MODULE_DEPTH));
        }
        self.loading.push(index);
        let includes = self.modules.get(index).map(|m| m.includes.clone()).unwrap_or_default();
        let folder = self.module_path(index).parent().map(Path::to_path_buf).unwrap_or_default();
        let mut targets = Vec::new();
        for (i, include) in includes.iter().enumerate() {
            let name = include.name();
            check_module_name(include)?;
            if let Some(earlier) = includes.iter().take(i).find(|earlier| earlier.name() == name) {
                let at = earlier.name_span;
                let message = if earlier.path == include.path {
                    format!("this module is already included at {}:{}; a file includes each module once", at.line, at.col)
                } else {
                    format!("another module named '{}' is already included at {}:{}; a file can't include two modules with one name", name, at.line, at.col)
                };
                return err(include.name_span, message);
            }
            let path = module_file(&folder, &include.path);
            let target = match self.by_path.get(&path) {
                Some(&target) => {
                    if let Some(start) = self.loading.iter().position(|&m| m == target) {
                        return err(include.span, self.cycle(start, target));
                    }
                    target
                }
                None => {
                    if let Some(other) = self.modules.iter().position(|m| !m.name.is_empty() && m.name == name) {
                        // Named from the main file's folder, as the program's paths are.
                        let main_folder = self.module_path(0).parent().unwrap_or(Path::new(""));
                        let other = self.module_path(other);
                        return err(
                            include.name_span,
                            format!(
                                "another module of this program is also named '{}' ({}); modules need different names",
                                name,
                                other.strip_prefix(main_folder).unwrap_or(other).display()
                            ),
                        );
                    }
                    let source = loader(&path).map_err(|error| {
                        let message = match error.kind() {
                            io::ErrorKind::NotFound => format!("can't find module '{}': there is no file '{}.vela'", include.written(), include.written()),
                            _ => format!("can't read module '{}' ({}): {}", include.written(), path.display(), error),
                        };
                        Error::at(include.span, message)
                    })?;
                    let target = self.parse(name.to_string(), &path, source, Some(include.span))?;
                    self.visit(target, loader)?;
                    target
                }
            };
            targets.push(target);
        }
        if let Some(module) = self.modules.get_mut(index) {
            module.included = targets;
        }
        self.loading.pop();
        self.order.push(index);
        Ok(())
    }

    fn module_path(&self, index: usize) -> &Path {
        self.modules.get(index).and_then(|m| self.files.get(m.file)).map_or(Path::new(""), |f| f.path.as_path())
    }

    /// The message for a module that includes itself: the modules being loaded
    /// from `start`, then `target` again.
    fn cycle(&self, start: usize, target: usize) -> String {
        let name = |index: usize| match self.modules.get(index) {
            Some(module) if module.name.is_empty() => self.module_path(index).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            Some(module) => module.name.clone(),
            None => String::new(),
        };
        let chain: Vec<String> = self.loading.iter().skip(start).map(|&m| name(m)).chain([name(target)]).collect();
        format!("modules can't include one another in a circle: {}", chain.join(" -> "))
    }

    /// Resolves the names in module `index`, and returns what the checker needs
    /// to know about it.
    fn resolve(&mut self, index: usize, tables: &[Names]) -> Result<ModuleScope> {
        let Some(module) = self.modules.get(index) else { return Ok(ModuleScope::default()) };
        let own = tables.get(index).cloned().unwrap_or_default();
        let mut resolver = Resolver { own, aliases: HashMap::new(), imports: HashMap::new(), type_places: &self.type_places };
        let mut imports = Vec::new();
        for (include, &target) in module.includes.iter().zip(&module.included) {
            let name = include.name().to_string();
            if let Some(entry) = resolver.own.get(&name) {
                return err(
                    include.name_span,
                    format!("the module '{}' has the name of the {} defined at {}:{}; rename one of them", name, entry.what.word(), entry.span.line, entry.span.col),
                );
            }
            let table = tables.get(target).cloned().unwrap_or_default();
            resolver.aliases.insert(name.clone(), table);
        }
        for include in &module.includes {
            let module_name = include.name();
            for (written, at) in &include.names {
                let Some(entry) = resolver.aliases.get(module_name).and_then(|table| table.get(written)) else {
                    return err(*at, format!("module '{}' has no task, shape, kind, or variant named '{}'", module_name, written));
                };
                if entry.private {
                    return err(*at, format!("'{}' is private to module '{}'", written, module_name));
                }
                if let Some(own) = resolver.own.get(written) {
                    return err(*at, format!("'{}' is brought in here, but this file also defines a {} with that name at {}:{}", written, own.what.word(), own.span.line, own.span.col));
                }
                if let Some((_, earlier)) = resolver.imports.get(written) {
                    return err(*at, format!("'{}' is already brought in at {}:{}", written, earlier.line, earlier.col));
                }
                if resolver.aliases.contains_key(written) {
                    return err(*at, format!("'{}' is the name of a module this file includes", written));
                }
                let entry = entry.clone();
                imports.push((written.clone(), entry.full.clone(), *at));
                resolver.imports.insert(written.clone(), (entry, *at));
            }
        }
        let scope = ModuleScope {
            name: module.name.clone(),
            file: module.file,
            includes: module.includes.iter().map(|u| (u.name().to_string(), u.name_span)).collect(),
            imports,
        };
        let Some(module) = self.modules.get_mut(index) else { return Ok(scope) };
        let mut items = std::mem::take(&mut module.items);
        for item in &mut items {
            resolver.item(item)?;
        }
        if let Some(module) = self.modules.get_mut(index) {
            module.items = items;
        }
        Ok(scope)
    }
}

/// Rejects a module named like a builtin type or task, which its qualified
/// names would read as.
fn check_module_name(include: &Include) -> Result<()> {
    let name = include.name();
    if Type::builtin(name).is_some() {
        return err(include.name_span, format!("a module can't be named '{}', which is a builtin type", name));
    }
    if BUILTIN_TASKS.contains(&name) {
        return err(include.name_span, format!("a module can't be named '{}', which is a builtin task", name));
    }
    Ok(())
}

/// `path` with `.` removed and each `..` taking away the folder before it,
/// where there is one: the same file always gets the same path.
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if matches!(parts.last(), Some(Component::Normal(_))) => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// What a name declared in a module is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Task,
    Shape,
    Kind,
    Variant,
}

impl What {
    fn word(self) -> &'static str {
        match self {
            What::Task => "task",
            What::Shape => "shape",
            What::Kind => "kind",
            What::Variant => "variant",
        }
    }
}

/// A task, shape, kind, or variant declared in a module.
#[derive(Debug, Clone)]
struct Entry {
    /// Its name in the whole program: `geometry.area`, or `area` in the main file.
    full: String,
    what: What,
    /// Whether it is `priv`; a variant is as private as its kind.
    private: bool,
    span: Span,
}

/// A module's declarations, by the names they are written with.
type Names = HashMap<String, Entry>;

/// The names module `module` declares. When two items share a name, the first
/// is kept; the checker reports the second.
fn own_names(module: &Module) -> Names {
    let full = |name: &str| if module.name.is_empty() { name.to_string() } else { format!("{}{}{}", module.name, MODULE_SEPARATOR, name) };
    let mut names = Names::new();
    let mut add = |name: &str, what: What, private: bool, span: Span| {
        names.entry(name.to_string()).or_insert(Entry { full: full(name), what, private, span });
    };
    for item in &module.items {
        match item {
            Item::Task(t) => add(&t.name, What::Task, t.private, t.span),
            Item::Shape(s) => add(&s.name, What::Shape, s.private, s.span),
            Item::Kind(k) => {
                add(&k.name, What::Kind, k.private, k.span);
                for variant in &k.variants {
                    add(&variant.tag, What::Variant, k.private, variant.span);
                }
            }
        }
    }
    names
}

/// Rewrites the names written in one module to their names in the program.
struct Resolver<'a> {
    own: Names,
    /// The declarations of each module this one includes, by the module's name.
    aliases: HashMap<String, Names>,
    /// The names brought in with `include m (a, b)`, with where each is brought in.
    imports: HashMap<String, (Entry, Span)>,
    type_places: &'a TypePlaces,
}

impl Resolver<'_> {
    fn item(&mut self, item: &mut Item) -> Result<()> {
        match item {
            Item::Task(t) => {
                t.name = self.own_full(&t.name);
                for param in &mut t.params {
                    param.ty = self.ty(&param.ty, param.ty_span)?;
                }
                let spans: Vec<Span> = (0..t.results.len().max(1)).map(|i| t.result_span(i)).collect();
                t.ret = match &t.ret {
                    Type::Results(types) => {
                        let mut resolved = Vec::new();
                        for (i, ty) in types.iter().enumerate() {
                            resolved.push(self.ty(ty, spans.get(i).copied().unwrap_or(t.span))?);
                        }
                        Type::Results(resolved)
                    }
                    other => self.ty(other, spans.first().copied().unwrap_or(t.span))?,
                };
                self.block(&mut t.body)
            }
            Item::Shape(s) => {
                s.name = self.own_full(&s.name);
                for field in &mut s.fields {
                    field.ty = self.ty(&field.ty, field.ty_span)?;
                }
                Ok(())
            }
            Item::Kind(k) => {
                k.name = self.own_full(&k.name);
                for variant in &mut k.variants {
                    for field in &mut variant.payload {
                        field.ty = self.ty(&field.ty, field.ty_span)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// The program's name for an item this module declares.
    fn own_full(&self, name: &str) -> String {
        self.own.get(name).map_or_else(|| name.to_string(), |entry| entry.full.clone())
    }

    /// The declaration a written name stands for: `m.Name` in an included module,
    /// or a name of this module or brought in with `include`. `None` for any other
    /// name, which the checker reports (or which is a variable). `at` is where
    /// the name is written, for errors.
    fn lookup(&self, written: &str, at: Span) -> Result<Option<Entry>> {
        let Some((module, member)) = written.split_once(MODULE_SEPARATOR) else {
            return Ok(self.own.get(written).or_else(|| self.imports.get(written).map(|(entry, _)| entry)).cloned());
        };
        let Some(entry) = self.aliases.get(module).and_then(|table| table.get(member)) else {
            return err(at, format!("module '{}' has no task, shape, kind, or variant named '{}'", module, member));
        };
        if entry.private {
            return err(at, format!("'{}' is private to module '{}'", member, module));
        }
        Ok(Some(entry.clone()))
    }

    /// `name` rewritten, when it stands for a declaration of one of the kinds `whats`.
    fn rename(&self, name: &mut String, at: Span, whats: &[What]) -> Result<()> {
        if let Some(entry) = self.lookup(name, at)?
            && (whats.contains(&entry.what) || name.contains(MODULE_SEPARATOR))
        {
            *name = entry.full;
        }
        Ok(())
    }

    /// A written type, at `span`, with its shape and kind names rewritten.
    fn ty(&self, ty: &Type, span: Span) -> Result<Type> {
        Ok(match ty {
            Type::Named(name, region) => {
                let at = self
                    .type_places
                    .get(&span.place_key())
                    .and_then(|parts| parts.iter().find(|(part, _)| part == name))
                    .map_or(span, |(_, at)| *at);
                let mut name = name.clone();
                self.rename(&mut name, at, &[What::Shape, What::Kind])?;
                Type::Named(name, region.clone())
            }
            Type::Ptr(inner, region) => Type::Ptr(Box::new(self.ty(inner, span)?), region.clone()),
            Type::Maybe(inner) => Type::Maybe(Box::new(self.ty(inner, span)?)),
            Type::Array(inner, count) => Type::Array(Box::new(self.ty(inner, span)?), *count),
            Type::Slice(inner, region) => Type::Slice(Box::new(self.ty(inner, span)?), region.clone()),
            Type::Range(inner) => Type::Range(Box::new(self.ty(inner, span)?)),
            Type::Fail(inner) => Type::Fail(Box::new(self.ty(inner, span)?)),
            Type::Or(value, error) => Type::Or(Box::new(self.ty(value, span)?), Box::new(self.ty(error, span)?)),
            Type::Results(types) => Type::Results(types.iter().map(|t| self.ty(t, span)).collect::<Result<_>>()?),
            other => other.clone(),
        })
    }

    fn block(&self, block: &mut Block) -> Result<()> {
        block.iter_mut().try_for_each(|stmt| self.stmt(stmt))
    }

    fn stmt(&self, stmt: &mut Stmt) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Binding { names, init, .. } => {
                for bound in names.iter_mut() {
                    if let (Some(ty), Some(at)) = (&bound.ty, bound.ty_span) {
                        bound.ty = Some(self.ty(ty, at)?);
                    }
                }
                init.iter_mut().try_for_each(|e| self.expr(e))
            }
            StmtKind::Assign(targets, values) => {
                for target in targets.iter_mut() {
                    if let Target::Place(e) = target {
                        self.expr(e)?;
                    }
                }
                values.iter_mut().try_for_each(|e| self.expr(e))
            }
            StmtKind::When(cond, then_block, else_block) => {
                self.expr(&mut cond.value)?;
                self.block(then_block)?;
                match else_block {
                    Some(block) => self.block(block),
                    None => Ok(()),
                }
            }
            StmtKind::Loop(cond, body) => {
                self.expr(&mut cond.value)?;
                self.block(body)
            }
            StmtKind::Each(_, range, body) => {
                self.expr(range)?;
                self.block(body)
            }
            StmtKind::Match(value, cases) => {
                self.expr(value)?;
                cases.iter_mut().try_for_each(|case| self.block(&mut case.body))
            }
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            StmtKind::Region(_) => Ok(()),
        }
    }

    fn expr(&self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit => Ok(()),
            // A variable keeps its name. No variable can be named like a variant
            // (see `Checker::define`), so a variant written as a value is renamed.
            ExprKind::Var(name) => self.rename(name, span, &[What::Variant]),
            ExprKind::Call(name, args) => {
                self.rename(name, span, &[What::Task, What::Variant])?;
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            ExprKind::StructInit(name, fields) => {
                self.rename(name, span, &[What::Shape, What::Kind])?;
                fields.iter_mut().try_for_each(|field| self.expr(&mut field.value))
            }
            ExprKind::Construct { args, .. } => args.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Field(base, _) | ExprKind::Deref(base) | ExprKind::Unary(_, base) | ExprKind::Fail(base) | ExprKind::Wrap(base) => self.expr(base),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Array(values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::New { ty, ty_span, value, .. } => {
                **ty = self.ty(ty, *ty_span)?;
                self.expr(value)
            }
            ExprKind::PlanB { value, body, .. } => {
                self.expr(value)?;
                self.block(body)
            }
        }
    }
}

fn err<T>(span: Span, message: impl Into<String>) -> Result<T> {
    Err(Error::at(span, message))
}
