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
    /// The module's identity, the prefix of its items' names in the program:
    /// its path from the main file's folder, without `.vela` (`shapes/geometry`);
    /// empty for the main file.
    pub name: String,
    /// The file it is.
    pub file: usize,
    /// The modules it includes.
    pub includes: Vec<IncludeScope>,
    /// The names its `include` lines bring in by name: each as this file writes
    /// it (its alias, if it has one), the name it stands for (`m.a`), and the
    /// position where it is brought in.
    pub imports: Vec<(String, String, Span)>,
    /// How this file writes the names of the program.
    pub spelling: Spelling,
}

/// A module that another includes.
#[derive(Debug, Clone, Default)]
pub struct IncludeScope {
    /// The name the including file names it by, if any: its alias or the
    /// path's last part; none when only its parts are included.
    pub local: Option<String>,
    /// Its identity (see `ModuleScope::name`).
    pub id: String,
    /// The path's last part, which names it unless it has an alias.
    pub file_name: String,
    /// The path as written.
    pub written: String,
    /// Where the name it is named by is written (or its path's last part).
    pub span: Span,
}

impl ModuleScope {
    /// The name that `name`, written unqualified in this module, stands for, if
    /// it is one brought in with `include`.
    pub fn imported(&self, name: &str) -> Option<&str> {
        self.imports.iter().find(|(written, _, _)| written == name).map(|(_, full, _)| full.as_str())
    }

    /// The module this one includes under the name `name`.
    pub fn included(&self, name: &str) -> Option<&IncludeScope> {
        self.includes.iter().find(|include| include.local.as_deref() == Some(name))
    }
}

/// How one file writes the names of the program's tasks, shapes, kinds, and
/// variants. A name of the program is its module's identity, a `.`, and its
/// own name (`shapes/geometry.Point`), or its own name alone in the main file.
/// A file writes it as its own name when it declares it or brings it in by
/// name (or by the alias it gives it), through its module's name otherwise
/// (`geometry.Point`), or through its kind's name for a variant (`Shape.Circle`).
/// A name the file has no way to write keeps the program's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spelling {
    names: HashMap<String, String>,
}

impl Spelling {
    /// How the file writes the program's name `full`.
    pub fn name(&self, full: &str) -> String {
        self.names.get(full).cloned().unwrap_or_else(|| full.to_string())
    }

    /// `ty` with its shape and kind names as the file writes them.
    pub fn ty(&self, ty: &Type) -> Type {
        ty.map_names(&|name| self.name(name))
    }

    /// `text`, a message, with each of the program's names in it as the file
    /// writes it. A name is a whole word of letters, digits, `_`, `.`, and `/`,
    /// which no other part of a message is written with.
    pub fn text(&self, text: &str) -> String {
        if self.names.is_empty() {
            return text.to_string();
        }
        let in_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/');
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find(in_word) {
            let (before, from) = rest.split_at(start);
            out.push_str(before);
            let end = from.find(|c: char| !in_word(c)).unwrap_or(from.len());
            let (word, after) = from.split_at(end);
            // A word may end a sentence, as in "… Point.".
            let trimmed = word.trim_end_matches('.');
            match self.names.get(trimmed) {
                Some(spelled) => {
                    out.push_str(spelled);
                    out.push_str(word.get(trimmed.len()..).unwrap_or_default());
                }
                None => out.push_str(word),
            }
            rest = after;
        }
        out.push_str(rest);
        out
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
    /// Its identity (see `ModuleScope::name`); empty for the main file.
    id: String,
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
        let variants = variants_of_kinds(&tables);
        let mut scopes = Vec::new();
        for index in 0..self.modules.len() {
            scopes.push(self.resolve(index, &tables, &variants)?);
        }
        let mut items = Vec::new();
        for index in std::mem::take(&mut self.order) {
            if let Some(module) = self.modules.get_mut(index) {
                items.append(&mut module.items);
            }
        }
        Ok(Program { items, modules: scopes, type_places: std::mem::take(&mut self.type_places) })
    }

    /// Lexes and parses `source` as the next file, the module with identity
    /// `id`, loaded by the `include` at `included_at`.
    fn parse(&mut self, id: String, path: &Path, source: String, included_at: Option<Span>) -> Result<usize> {
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
        self.modules.push(Module { id, file, includes, included: Vec::new(), items });
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
        let main_folder = self.module_path(0).parent().map(Path::to_path_buf).unwrap_or_default();
        let mut targets = Vec::new();
        for (i, include) in includes.iter().enumerate() {
            if let Some(local) = include.local_name() {
                check_module_name(local, include.local_span())?;
            }
            let path = module_file(&folder, &include.path);
            let earlier = includes.iter().take(i);
            if let Some(earlier) = earlier.clone().find(|earlier| module_file(&folder, &earlier.path) == path) {
                let at = earlier.name_span;
                return err(include.name_span, format!("this module is already included at {}:{}; a file includes each module once", at.line, at.col));
            }
            if let Some(local) = include.local_name()
                && let Some(earlier) = earlier.clone().find(|earlier| earlier.local_name() == Some(local))
            {
                let at = earlier.local_span();
                return err(
                    include.local_span(),
                    format!(
                        "another module named '{}' is already included at {}:{}; a file can't include two modules with one name, so give one of them another name with '=> name'",
                        local, at.line, at.col
                    ),
                );
            }
            let target = match self.by_path.get(&path) {
                Some(&target) => {
                    if let Some(start) = self.loading.iter().position(|&m| m == target) {
                        return err(include.span, self.cycle(start, target));
                    }
                    target
                }
                None => {
                    let source = loader(&path).map_err(|error| {
                        let message = match error.kind() {
                            io::ErrorKind::NotFound => format!("can't find module '{}': there is no file '{}.vela'", include.written(), include.written()),
                            _ => format!("can't read module '{}' ({}): {}", include.written(), path.display(), error),
                        };
                        Error::at(include.span, message)
                    })?;
                    let target = self.parse(module_id(&main_folder, &path), &path, source, Some(include.span))?;
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
    /// from `start`, then `target` again, each by its path from the main file's
    /// folder (the main file by its name).
    fn cycle(&self, start: usize, target: usize) -> String {
        let name = |index: usize| match self.modules.get(index) {
            Some(module) if module.id.is_empty() => self.module_path(index).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            Some(module) => module.id.clone(),
            None => String::new(),
        };
        let chain: Vec<String> = self.loading.iter().skip(start).map(|&m| name(m)).chain([name(target)]).collect();
        format!("modules can't include one another in a circle: {}", chain.join(" -> "))
    }

    /// Resolves the names in module `index`, and returns what the checker needs
    /// to know about it.
    fn resolve(&mut self, index: usize, tables: &[Names], variants: &Variants) -> Result<ModuleScope> {
        let Some(module) = self.modules.get(index) else { return Ok(ModuleScope::default()) };
        let own = tables.get(index).cloned().unwrap_or_default();
        let mut resolver = Resolver { own, modules: HashMap::new(), imports: HashMap::new(), unnamed: Vec::new(), variants, type_places: &self.type_places };
        let mut includes = Vec::new();
        for (include, &target) in module.includes.iter().zip(&module.included) {
            let id = self.modules.get(target).map(|m| m.id.clone()).unwrap_or_default();
            if let Some(local) = include.local_name() {
                if let Some(entry) = resolver.own.get(local) {
                    return err(
                        include.local_span(),
                        format!(
                            "the module '{}' has the name of the {} defined at {}:{}; rename one of them, or give the module another name with '=> name'",
                            local,
                            entry.what.word(),
                            entry.span.line,
                            entry.span.col
                        ),
                    );
                }
                // A part given another name is named only by it.
                let hidden = include.parts.iter().filter_map(|part| part.alias.clone().map(|alias| (part.name.clone(), alias))).collect();
                let names = tables.get(target).cloned().unwrap_or_default();
                resolver.modules.insert(local.to_string(), ModuleNames { names, hidden });
            }
            if include.local_name() != Some(include.file_name()) {
                resolver.unnamed.push((include.file_name().to_string(), include.local_name().map(str::to_string), include.written()));
            }
            includes.push(IncludeScope {
                local: include.local_name().map(str::to_string),
                id,
                file_name: include.file_name().to_string(),
                written: include.written(),
                span: include.local_span(),
            });
        }
        let mut imports = Vec::new();
        for (include, &target) in module.includes.iter().zip(&module.included) {
            let label = include.local_name().map_or_else(|| include.written(), str::to_string);
            let table = tables.get(target);
            for part in &include.parts {
                let Some(entry) = table.and_then(|table| table.get(&part.name)) else {
                    return err(part.span, format!("module '{}' has no task, shape, kind, or variant named '{}'", label, part.name));
                };
                if entry.private {
                    return err(part.span, format!("'{}' is private to module '{}'", part.name, label));
                }
                if let Some((alias, at)) = &part.alias {
                    if entry.what == What::Variant {
                        let kind = entry.kind.as_deref().map_or("Kind", unqualified);
                        return err(
                            *at,
                            format!("'{0}' is a variant, which keeps its own name; bring in '{0}' as it is, or its kind, and write '{1}.{0}'", part.name, kind),
                        );
                    }
                    if *alias == part.name {
                        return err(*at, format!("'{0}' is already named '{0}'; leave out '=> {0}'", alias));
                    }
                    if Type::builtin(alias).is_some() {
                        return err(*at, format!("'{}' is a builtin type, so nothing else can be named so", alias));
                    }
                    if BUILTIN_TASKS.contains(&alias.as_str()) {
                        return err(*at, format!("'{}' is a builtin task, so nothing else can be named so", alias));
                    }
                }
                let (written, at) = (part.local_name(), part.local_span());
                if let Some(own) = resolver.own.get(written) {
                    return err(at, format!("'{}' is brought in here, but this file also defines a {} with that name at {}:{}", written, own.what.word(), own.span.line, own.span.col));
                }
                if let Some((_, earlier)) = resolver.imports.get(written) {
                    return err(at, format!("'{}' is already brought in at {}:{}", written, earlier.line, earlier.col));
                }
                if resolver.modules.contains_key(written) {
                    return err(at, format!("'{}' is the name of a module this file includes", written));
                }
                let entry = entry.clone();
                imports.push((written.to_string(), entry.full.clone(), at));
                resolver.imports.insert(written.to_string(), (entry, at));
            }
        }
        let spelling = spelling(module, tables.get(index), tables, variants, &imports);
        let scope = ModuleScope { name: module.id.clone(), file: module.file, includes, imports, spelling };
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

/// How module `module`, which declares `own`, writes the program's names (see
/// `Spelling`), given the names its `include` lines bring in (`imports`, as in
/// `ModuleScope`).
fn spelling(module: &Module, own: Option<&Names>, tables: &[Names], variants: &Variants, imports: &[(String, String, Span)]) -> Spelling {
    let mut names = HashMap::new();
    if let Some(own) = own {
        for (written, entry) in own {
            names.insert(entry.full.clone(), written.clone());
        }
    }
    for (include, &target) in module.includes.iter().zip(&module.included) {
        let (Some(local), Some(table)) = (include.local_name(), tables.get(target)) else { continue };
        for (written, entry) in table {
            let renamed = include.parts.iter().any(|part| part.name == *written && part.alias.is_some());
            if !entry.private && !renamed {
                names.entry(entry.full.clone()).or_insert_with(|| format!("{}{}{}", local, MODULE_SEPARATOR, written));
            }
        }
    }
    for (written, full, _) in imports {
        names.insert(full.clone(), written.clone());
    }
    // A variant not written otherwise is written through its kind.
    for (kind, tags) in variants {
        let Some(kind) = names.get(kind).cloned() else { continue };
        for (tag, entry) in tags {
            names.entry(entry.full.clone()).or_insert_with(|| format!("{}{}{}", kind, MODULE_SEPARATOR, tag));
        }
    }
    // The main file's names are already written as the program names them.
    names.retain(|full, written| full != written);
    Spelling { names }
}

/// Rejects a module named like a builtin type or task in a file, `name` written
/// at `span`, which its qualified names would read as.
fn check_module_name(name: &str, span: Span) -> Result<()> {
    if Type::builtin(name).is_some() {
        return err(span, format!("a module can't be named '{}', which is a builtin type", name));
    }
    if BUILTIN_TASKS.contains(&name) {
        return err(span, format!("a module can't be named '{}', which is a builtin task", name));
    }
    Ok(())
}

/// The identity of the module in the file at `path`: its path from the main
/// file's folder `main_folder`, with `/` between the parts and without
/// `.vela`, as in `shapes/geometry` or `../common/text`.
pub fn module_id(main_folder: &Path, path: &Path) -> String {
    let folder = normalize(main_folder);
    let folder: Vec<Component> = folder.components().collect();
    let file = normalize(&path.with_extension(""));
    let parts: Vec<Component> = file.components().collect();
    let common = folder.iter().zip(&parts).take_while(|(a, b)| a == b).count();
    let ups = folder.len().saturating_sub(common);
    let mut id: Vec<String> = vec!["..".to_string(); ups];
    id.extend(parts.iter().skip(common).map(|part| part.as_os_str().to_string_lossy().into_owned()));
    id.join("/")
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
    /// Its name in the whole program: `shapes/geometry.area`, or `area` in the
    /// main file.
    full: String,
    what: What,
    /// Whether it is `priv`; a variant is as private as its kind.
    private: bool,
    span: Span,
    /// For a variant, its kind's name in the program.
    kind: Option<String>,
}

/// A module's declarations, by the names they are written with.
type Names = HashMap<String, Entry>;

/// The variants of every kind of the program, by the kind's name in the
/// program, then the variant's own name.
type Variants = HashMap<String, HashMap<String, Entry>>;

/// The names module `module` declares. When two items share a name, the first
/// is kept; the checker reports the second.
fn own_names(module: &Module) -> Names {
    let full = |name: &str| if module.id.is_empty() { name.to_string() } else { format!("{}{}{}", module.id, MODULE_SEPARATOR, name) };
    let mut names = Names::new();
    let mut add = |name: &str, what: What, private: bool, span: Span, kind: Option<String>| {
        names.entry(name.to_string()).or_insert(Entry { full: full(name), what, private, span, kind });
    };
    for item in &module.items {
        match item {
            Item::Task(t) => add(&t.name, What::Task, t.private, t.span, None),
            Item::Shape(s) => add(&s.name, What::Shape, s.private, s.span, None),
            Item::Kind(k) => {
                add(&k.name, What::Kind, k.private, k.span, None);
                for variant in &k.variants {
                    add(&variant.tag, What::Variant, k.private, variant.span, Some(full(&k.name)));
                }
            }
        }
    }
    names
}

/// The variants of every kind in `tables` (see `Variants`).
fn variants_of_kinds(tables: &[Names]) -> Variants {
    let mut variants = Variants::new();
    for (tag, entry) in tables.iter().flatten() {
        if let (What::Variant, Some(kind)) = (entry.what, &entry.kind) {
            variants.entry(kind.clone()).or_default().entry(tag.clone()).or_insert_with(|| entry.clone());
        }
    }
    variants
}

/// What a file can name of a module it includes under a name.
struct ModuleNames {
    names: Names,
    /// The parts brought in under another name, which the module's name can't
    /// reach: each part's name, with its alias and where that is written.
    hidden: HashMap<String, Binding>,
}

/// Rewrites the names written in one module to their names in the program.
struct Resolver<'a> {
    own: Names,
    /// The modules this one includes, by the name it names each by.
    modules: HashMap<String, ModuleNames>,
    /// The names brought in by name, by the name this file writes (an alias if
    /// there is one), with where each is brought in.
    imports: HashMap<String, (Entry, Span)>,
    /// The modules this one includes that its path's last part doesn't name:
    /// that part, the name it is included under (if any), and the path.
    unnamed: Vec<(String, Option<String>, String)>,
    variants: &'a Variants,
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

    /// The declaration a written name stands for: a name of this module or one
    /// brought in by name; `m.Name` in a module this file includes;
    /// `Kind.Variant` or `m.Kind.Variant`. `None` for an undotted name that
    /// names none of them, which the checker reports (or which is a variable).
    /// `at` is where the name is written, for errors.
    fn lookup(&self, written: &str, at: Span) -> Result<Option<Entry>> {
        let parts: Vec<&str> = written.split(MODULE_SEPARATOR).collect();
        match parts.as_slice() {
            [name] => Ok(self.own.get(*name).or_else(|| self.imports.get(*name).map(|(entry, _)| entry)).cloned()),
            [first, member] => match self.modules.get(*first) {
                Some(module) => self.member(first, module, member, at).map(Some),
                None => match self.lookup(first, at)? {
                    Some(kind) if kind.what == What::Kind => self.variant(&kind, first, member, at).map(Some),
                    _ => {
                        let hint = match self.unnamed.iter().find(|(file_name, _, _)| file_name == first) {
                            Some((_, Some(alias), path)) => format!("; module '{}' is included as '{}'", path, alias),
                            Some((_, None, path)) => format!("; module '{}' is included with only its parts, which are named alone", path),
                            None => ", and Vela has no methods".to_string(),
                        };
                        err(at, format!("'{}' names nothing: '{}' is neither a module this file includes nor a kind{}", written, first, hint))
                    }
                },
            },
            [module_name, kind_name, variant] => {
                let Some(module) = self.modules.get(*module_name) else {
                    return err(at, format!("'{}' names nothing: '{}' isn't a module this file includes", written, module_name));
                };
                let kind = self.member(module_name, module, kind_name, at)?;
                if kind.what != What::Kind {
                    return err(at, format!("'{}{}{}' is a {}, not a kind, so it has no variant '{}'", module_name, MODULE_SEPARATOR, kind_name, kind.what.word(), variant));
                }
                self.variant(&kind, &format!("{}{}{}", module_name, MODULE_SEPARATOR, kind_name), variant, at).map(Some)
            }
            _ => err(at, format!("'{}' names nothing", written)),
        }
    }

    /// `member` of the module this file names `module_name`.
    fn member(&self, module_name: &str, module: &ModuleNames, member: &str, at: Span) -> Result<Entry> {
        if let Some((alias, brought)) = module.hidden.get(member) {
            return err(
                at,
                format!(
                    "'{}{}{}' is brought in as '{}' at {}:{}, so this file names it '{}'",
                    module_name, MODULE_SEPARATOR, member, alias, brought.line, brought.col, alias
                ),
            );
        }
        let Some(entry) = module.names.get(member) else {
            return err(at, format!("module '{}' has no task, shape, kind, or variant named '{}'", module_name, member));
        };
        if entry.private {
            return err(at, format!("'{}' is private to module '{}'", member, module_name));
        }
        Ok(entry.clone())
    }

    /// The variant `tag` of `kind`, which this file writes `kind_name`.
    fn variant(&self, kind: &Entry, kind_name: &str, tag: &str, at: Span) -> Result<Entry> {
        match self.variants.get(&kind.full).and_then(|tags| tags.get(tag)) {
            Some(entry) => Ok(entry.clone()),
            None => err(at, format!("kind '{}' has no variant '{}'", kind_name, tag)),
        }
    }

    /// The kind `name` (as written, maybe `m.Kind`) stands for, if it names one.
    fn kind(&self, name: &str, at: Span) -> Result<Option<Entry>> {
        if !name.contains(MODULE_SEPARATOR) || name.split(MODULE_SEPARATOR).count() == 2 && self.modules.contains_key(module_of(name)) {
            return Ok(self.lookup(name, at)?.filter(|entry| entry.what == What::Kind));
        }
        Ok(None)
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

    /// A pattern's kind, as in `case Shape.Circle`, rewritten to its name in the
    /// program.
    fn pattern(&self, pattern: &mut Pattern) -> Result<()> {
        if let PatternKind::Variant { kind: Some(kind), .. } = &mut pattern.kind {
            let (name, at) = &mut **kind;
            match self.kind(name, *at)? {
                Some(kind) => *name = kind.full,
                None => return err(*at, format!("'{}' isn't a kind, so a pattern can't name a variant of it", name)),
            }
        }
        Ok(())
    }

    /// `Shape.Circle` (or `m.Shape.Circle`), a variant read as a field of a
    /// name, rewritten to the variant; whether `expr` was one. No variable can
    /// be named like a kind (see `Checker::define`). (A function of its own:
    /// `expr`'s frame is on the stack once per nesting level.)
    fn variant_as_field(&self, expr: &mut Expr) -> Result<bool> {
        let ExprKind::Field(base, tag) = &expr.kind else { return Ok(false) };
        let ExprKind::Var(name) = &base.kind else { return Ok(false) };
        if let Some(kind) = self.kind(name, base.span)? {
            let variant = self.variant(&kind, name, tag, expr.span)?;
            expr.kind = ExprKind::Var(variant.full);
            return Ok(true);
        }
        // `m.Name.x`, where `m.Name` isn't a kind. (A variable may be named like
        // a shape or task of this file, so `Name.x` may be a field.)
        if name.contains(MODULE_SEPARATOR)
            && let Some(entry) = self.lookup(name, base.span)?
        {
            return err(expr.span, format!("'{}' is a {}, not a kind, so it has no variant '{}'", name, entry.what.word(), tag));
        }
        Ok(false)
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
                cond.pattern.iter_mut().try_for_each(|pattern| self.pattern(pattern))?;
                self.block(then_block)?;
                match else_block {
                    Some(block) => self.block(block),
                    None => Ok(()),
                }
            }
            StmtKind::Loop(cond, body) => {
                self.expr(&mut cond.value)?;
                cond.pattern.iter_mut().try_for_each(|pattern| self.pattern(pattern))?;
                self.block(body)
            }
            StmtKind::Each(_, range, body) => {
                self.expr(range)?;
                self.block(body)
            }
            StmtKind::Match(value, cases) => {
                self.expr(value)?;
                cases.iter_mut().try_for_each(|case| self.pattern(&mut case.pattern))?;
                cases.iter_mut().try_for_each(|case| self.block(&mut case.body))
            }
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            StmtKind::Region(_) => Ok(()),
        }
    }

    fn expr(&self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        if matches!(&expr.kind, ExprKind::Field(base, _) if matches!(base.kind, ExprKind::Var(_))) && self.variant_as_field(expr)? {
            return Ok(());
        }
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
