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
use crate::library;
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
            // A name may also start a longer word, as a kind does in `Kind.Variant`.
            let name = std::iter::once(trimmed.len())
                .chain(trimmed.rmatch_indices('.').map(|(at, _)| at))
                .find_map(|end| trimmed.get(..end).and_then(|name| self.names.get(name)).map(|spelled| (end, spelled)));
            match name {
                Some((end, spelled)) => {
                    out.push_str(spelled);
                    out.push_str(word.get(end..).unwrap_or_default());
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
    /// Each loaded file's module, by its `identity`.
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
        let main = self.parse(String::new(), path, identity(path), source.to_string(), None)?;
        self.visit(main, loader)?;
        let tables: Vec<Names> = self.modules.iter().map(own_names).collect();
        let variants = variants_of_kinds(&self.modules);
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
    /// `id`, read from `path`, which `key` tells from every other file, loaded by
    /// the `include` at `included_at`.
    fn parse(&mut self, id: String, path: &Path, key: PathBuf, source: String, included_at: Option<Span>) -> Result<usize> {
        let file = self.files.len();
        self.files.push(SourceFile { path: path.to_path_buf(), source, included_at });
        let source = self.files.get(file).map_or("", |f| f.source.as_str());
        let (tokens, spans) = Lexer::for_file(source, file).tokenize()?;
        let mut parser = Parser::new(tokens, spans);
        let items = parser.parse_file()?;
        check_natives(&id, &items)?;
        self.type_places.extend(parser.type_places());
        let includes = parser.includes();
        let index = self.modules.len();
        self.by_path.insert(key, index);
        self.modules.push(Module { id, file, includes, included: Vec::new(), items });
        Ok(index)
    }

    /// Loads every module that module `index` includes, and theirs, in order.
    fn visit(&mut self, index: usize, loader: &mut Loader) -> Result<()> {
        self.loading.push(index);
        let includes = self.modules.get(index).map(|m| m.includes.clone()).unwrap_or_default();
        let folder = self.module_path(index).parent().map(Path::to_path_buf).unwrap_or_default();
        let main_folder = self.module_path(0).parent().map(Path::to_path_buf).unwrap_or_default();
        let in_library = self.modules.get(index).is_some_and(|module| library::is_library(&module.id));
        let mut targets = Vec::new();
        for (i, include) in includes.iter().enumerate() {
            if let Some(local) = include.local_name() {
                check_module_name(local, include.local_span())?;
            }
            if let Some((alias, at)) = &include.alias
                && alias == include.file_name()
            {
                return err(*at, format!("'{0}' is already named '{0}'; leave out '=> {0}'", alias));
            }
            let from_library = library_module(include)?;
            if in_library && from_library.is_none() {
                return err(include.span, "a module of Vela's library includes only other modules of the library, as in 'include std/files'");
            }
            let path = match &from_library {
                Some((name, _)) => PathBuf::from(format!("{}/{}.vela", library::ROOT, name)),
                None => module_file(&folder, &include.path),
            };
            let key = include_key(&folder, include);
            let earlier = includes.iter().take(i);
            if let Some(earlier) = earlier.clone().find(|earlier| include_key(&folder, earlier) == key) {
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
            let target = match self.by_path.get(&key) {
                Some(&target) => {
                    if let Some(start) = self.loading.iter().position(|&m| m == target) {
                        return err(include.span, self.cycle(start, target));
                    }
                    target
                }
                None => {
                    if self.loading.len() >= MAX_MODULE_DEPTH {
                        return err(include.span, format!("modules include one another too deeply (more than {} in a chain)", MAX_MODULE_DEPTH));
                    }
                    if let Some((name, source)) = from_library {
                        let id = format!("{}/{}", library::ROOT, name);
                        let target = self.parse(id, &path, key, source.to_string(), Some(include.span))?;
                        self.visit(target, loader)?;
                        targets.push(target);
                        continue;
                    }
                    let id = module_id(&main_folder, &path);
                    if library::is_library(&id) {
                        return err(
                            include.span,
                            format!(
                                "'{}' is in a folder named '{}' beside the main file, which would read as Vela's library; give the folder another name",
                                include.written(),
                                library::ROOT
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
                    let target = self.parse(id, &path, key, source, Some(include.span))?;
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
        let mut own = tables.get(index).cloned().unwrap_or_default();
        // The builtin kinds are written like declared ones, in every file: a task
        // may send one (`fail ParseError.TooLarge`) as well as read one. A file
        // declaring such a name keeps its own entry here, so that the checker
        // reports the clash rather than this (see `check::collect_declarations`).
        for kind in builtin_kinds() {
            own.entry(kind.name.clone()).or_insert_with(|| builtin_kind_entry(&kind.name, What::Kind, None));
        }
        let mut resolver =
            Resolver { own, modules: HashMap::new(), imports: HashMap::new(), unnamed: Vec::new(), variants, type_places: &self.type_places, params: Vec::new() };
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
            for (i, part) in include.parts.iter().enumerate() {
                if let Some(earlier) = include.parts.iter().take(i).find(|earlier| earlier.name == part.name) {
                    return err(part.span, format!("'{}' is already listed at {}:{}; a name is brought in once", part.name, earlier.span.line, earlier.span.col));
                }
                let Some(entry) = table.and_then(|table| table.get(&part.name)) else {
                    return err(part.span, format!("module '{}' has no task, shape, kind, or variant named '{}'", label, part.name));
                };
                if entry.private {
                    return err(part.span, format!("'{}' is private to module '{}'", part.name, label));
                }
                if let Some((alias, at)) = &part.alias {
                    if entry.what == What::Variant {
                        let tag = entry.kind.as_deref().map_or("Kind", unqualified);
                        // The kind as this line would let the file write it.
                        let kind = match (include.parts.iter().find(|listed| listed.name == tag), include.local_name()) {
                            (Some(listed), _) => listed.local_name().to_string(),
                            (None, Some(local)) => format!("{}{}{}", local, MODULE_SEPARATOR, tag),
                            (None, None) => tag.to_string(),
                        };
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
                    if let Some(clash) = builtin_kind_clash(alias) {
                        return err(*at, clash);
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
        let spelling = spelling(module, tables.get(index), tables, variants, &imports, &includes);
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
/// `ModuleScope`) and the modules it includes (`includes`).
fn spelling(module: &Module, own: Option<&Names>, tables: &[Names], variants: &Variants, imports: &[(String, String, Span)], includes: &[IncludeScope]) -> Spelling {
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
    // A name the file can't write, of a module whose identity reads like the
    // name the file gives another module (`c.Thing` from `c.vela`, where
    // `include x => c`), starts with `./`, to tell them apart.
    let taken = |id: &str| includes.iter().any(|include| include.local.as_deref() == Some(id) && include.id != id);
    for (_, entry) in tables.iter().flatten() {
        if let Some((id, _)) = entry.full.rsplit_once(MODULE_SEPARATOR)
            && taken(id)
        {
            names.entry(entry.full.clone()).or_insert_with(|| format!("./{}", entry.full));
        }
    }
    // The main file's names are already written as the program names them.
    names.retain(|full, written| full != written);
    Spelling { names }
}

/// The module of Vela's library that `include` names, when its path starts with
/// `std`: its name after `std/` and its source.
fn library_module(include: &Include) -> Result<Option<(String, &'static str)>> {
    let Some((first, rest)) = include.path.split_first() else { return Ok(None) };
    if first != library::ROOT {
        return Ok(None);
    }
    if rest.is_empty() {
        return err(
            include.span,
            format!("'{0}' is Vela's library, not one of its modules; include a module of it, as in 'include {0}/files'", library::ROOT),
        );
    }
    let name = rest.join("/");
    match library::module(&name) {
        Some(source) => Ok(Some((name, source))),
        None => err(include.span, format!("Vela's library has no module '{}/{}'; its modules are {}", library::ROOT, name, library::listed())),
    }
}

/// What tells the module `include` loads, from a file in `folder`, from every
/// other: a module of the library by its name, which no file on the disk has,
/// and any other by the file it is.
fn include_key(folder: &Path, include: &Include) -> PathBuf {
    match include.path.first() {
        Some(first) if first == library::ROOT => PathBuf::from(format!("<{}>/{}", library::ROOT, include.path.join("/"))),
        _ => identity(&module_file(folder, &include.path)),
    }
}

/// Rejects a native task (see `ast::NATIVE`) outside Vela's library, and one of
/// the library whose body the runtime doesn't have, in the module with identity
/// `id` that declares `items`.
fn check_natives(id: &str, items: &[Item]) -> Result<()> {
    for item in items {
        let Item::Task(task) = item else { continue };
        if !task.native {
            continue;
        }
        if !library::is_library(id) {
            let fix = if task.body.is_empty() { format!("give '{}' a body in Vela, closed with 'fi'", task.name) } else { format!("leave out '{}'", NATIVE) };
            return err(task.span, format!("'{}' marks a task whose body the compiler writes in C, which only Vela's library has; {}", NATIVE, fix));
        }
        if let Some(first) = task.body.first() {
            return err(first.span, format!("native task '{}' has no body, since the compiler writes it in C; leave out its lines and its 'fi'", task.name));
        }
        let full = format!("{}{}{}", id, MODULE_SEPARATOR, task.name);
        if library::native(&full).is_none() {
            return Err(Error::internal_at(task.span, format!("the runtime has no body for the native task '{}' (see src/library.rs)", full)));
        }
    }
    Ok(())
}

/// Rejects a module named like a builtin type, task, or kind in a file, `name`
/// written at `span`, which its qualified names would read as.
fn check_module_name(name: &str, span: Span) -> Result<()> {
    if Type::builtin(name).is_some() {
        return err(span, format!("a module can't be named '{}', which is a builtin type", name));
    }
    if BUILTIN_TASKS.contains(&name) {
        return err(span, format!("a module can't be named '{}', which is a builtin task", name));
    }
    // A name of a builtin kind would stand for two things in the file: the
    // module, and the kind or variant every file writes (see `ast::builtin_kinds`).
    if let Some(kind) = builtin_kind_of(name) {
        return err(span, format!("a module can't be named '{}', which is a variant of the builtin kind '{}'", name, kind));
    }
    if BUILTIN_KIND_NAMES.contains(&name) {
        return err(span, format!("a module can't be named '{}', which is a builtin kind", name));
    }
    Ok(())
}

/// The identity of the module in the file at `path`: its path from the main
/// file's folder `main_folder`, with `/` between the parts and without
/// `.vela`, as in `shapes/geometry` or `../common/text`.
pub fn module_id(main_folder: &Path, path: &Path) -> String {
    let folder = identity(main_folder);
    let folder: Vec<Component> = folder.components().collect();
    let file = identity(&path.with_extension(""));
    let parts: Vec<Component> = file.components().collect();
    let common = folder.iter().zip(&parts).take_while(|(a, b)| a == b).count();
    let ups = folder.len().saturating_sub(common);
    let mut id: Vec<String> = vec!["..".to_string(); ups];
    id.extend(parts.iter().skip(common).map(|part| part.as_os_str().to_string_lossy().into_owned()));
    id.join("/")
}

/// The path that tells which file `path` is: from the working directory, when
/// there is one, and normalized. A relative `..` can climb above the folder a
/// program's paths start from, as in `../app/config` read from the folder
/// `app`, which is `config`; only the working directory knows that folder's
/// name. (Such a path can only be read from the disk: an editor keeps a
/// program inside its folder.)
fn identity(path: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(dir) => normalize(&dir.join(path)),
        Err(_) => normalize(path),
    }
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

/// An entry for a builtin kind or one of its variants, which no file declares,
/// so its position is in no file of the program (see `ast::builtin_kinds`).
fn builtin_kind_entry(name: &str, what: What, kind: Option<String>) -> Entry {
    Entry { full: name.to_string(), what, private: false, span: parse_error_kind().span, kind }
}

/// The message for a name a builtin kind or one of its variants owns, which a
/// part brought in under another name may not take either (see
/// `ast::builtin_kinds`, `check_module_name` for a module, and
/// `check::collect_declarations` for a declaration).
fn builtin_kind_clash(name: &str) -> Option<String> {
    if !BUILTIN_KIND_NAMES.contains(&name) {
        return None;
    }
    Some(match builtin_kind_of(name) {
        Some(kind) => format!("'{}' is a variant of the builtin kind '{}', so nothing else can be named so", name, kind),
        None => format!("'{}' is a builtin kind, so nothing else can be named so", name),
    })
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

/// The variants of every kind in `modules` (see `Variants`). Unlike a module's
/// names, a variant named like another declaration of its module is kept, so
/// that `Kind.Variant` finds it and the checker reports the clash.
fn variants_of_kinds(modules: &[Module]) -> Variants {
    let mut variants = Variants::new();
    // The variants of the builtin kinds, written through them as any kind's are.
    for kind in builtin_kinds() {
        let tags = variants.entry(kind.name.clone()).or_default();
        for variant in kind.variants {
            let entry = builtin_kind_entry(&variant.tag, What::Variant, Some(kind.name.clone()));
            tags.insert(variant.tag.clone(), entry);
        }
    }
    for module in modules {
        let full = |name: &str| if module.id.is_empty() { name.to_string() } else { format!("{}{}{}", module.id, MODULE_SEPARATOR, name) };
        for item in &module.items {
            let Item::Kind(k) = item else { continue };
            let tags = variants.entry(full(&k.name)).or_default();
            for variant in &k.variants {
                let entry = Entry { full: full(&variant.tag), what: What::Variant, private: k.private, span: variant.span, kind: Some(full(&k.name)) };
                tags.entry(variant.tag.clone()).or_insert(entry);
            }
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
    /// The names of the types the declaration being walked takes: they name no
    /// shape or kind, so a written type naming one is left as it is.
    params: Vec<String>,
}

impl Resolver<'_> {
    fn item(&mut self, item: &mut Item) -> Result<()> {
        match item {
            Item::Task(t) => {
                t.name = self.own_full(&t.name);
                self.enter_type_params(&t.types, "task", &t.name)?;
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
                let result = self.block(&mut t.body);
                self.params.clear();
                result
            }
            Item::Shape(s) => {
                s.name = self.own_full(&s.name);
                self.enter_type_params(&s.types, "shape", &s.name)?;
                for field in &mut s.fields {
                    field.ty = self.ty(&field.ty, field.ty_span)?;
                    // A field's default names the program's things as the body
                    // of a task does: `state: State = State.Idle`.
                    if let Some(value) = &mut field.default {
                        self.expr(value)?;
                    }
                }
                for member in &mut s.shared {
                    member.field.ty = self.ty(&member.field.ty, member.field.ty_span)?;
                    if let Some(value) = &mut member.field.default {
                        self.expr(value)?;
                    }
                }
                self.params.clear();
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

    /// Brings the types a declaration takes into scope, so the types written in
    /// it name them rather than a shape or kind of the same name, which would be
    /// two types under one name.
    fn enter_type_params(&mut self, types: &[TypeParam], what: &str, name: &str) -> Result<()> {
        for param in types {
            if let Some(entry) = self.lookup(&param.name, param.span, false)?
                && matches!(entry.what, What::Shape | What::Kind)
            {
                return err(
                    param.span,
                    format!("'{}' is already a type, so it can't name a type {} '{}' takes; use another name, as in '<T>'", param.name, what, unqualified(name)),
                );
            }
        }
        self.params = types.iter().map(|param| param.name.clone()).collect();
        Ok(())
    }

    /// The program's name for an item this module declares.
    fn own_full(&self, name: &str) -> String {
        self.own.get(name).map_or_else(|| name.to_string(), |entry| entry.full.clone())
    }

    /// The declaration a written name stands for: a name of this module or one
    /// brought in by name; `m.Name` in a module this file includes;
    /// `Kind.Variant` or `m.Kind.Variant`. `None` for an undotted name that
    /// names none of them, which the checker reports (or which is a variable).
    /// `at` is where the name is written, for errors, and `calls` whether it is
    /// called.
    fn lookup(&self, written: &str, at: Span, calls: bool) -> Result<Option<Entry>> {
        // The value a method is called on is named `.this`, which is no
        // declaration of any file and holds a `.` no name of one can (see
        // `ast::THIS`).
        if written == THIS {
            return Ok(None);
        }
        let parts: Vec<&str> = written.split(MODULE_SEPARATOR).collect();
        match parts.as_slice() {
            [name] => Ok(self.own.get(*name).or_else(|| self.imports.get(*name).map(|(entry, _)| entry)).cloned()),
            [first, member] => match self.modules.get(*first) {
                Some(module) => self.member(first, module, member, at).map(Some),
                None => match self.lookup(first, at, calls)? {
                    Some(kind) if kind.what == What::Kind => self.variant(&kind, first, member, at).map(Some),
                    _ => {
                        let hint = match self.kind_hint(first) {
                            hint if hint.is_empty() => self.unnamed_hint(written, first, calls),
                            hint => hint,
                        };
                        err(at, format!("'{}' names nothing: '{}' is neither a module this file includes nor a kind{}", written, first, hint))
                    }
                },
            },
            [module_name, kind_name, variant] => {
                let Some(module) = self.modules.get(*module_name) else {
                    return err(at, format!("'{}' names nothing: '{}' isn't a module this file includes{}", written, module_name, self.unnamed_hint(written, module_name, calls)));
                };
                let kind = self.member(module_name, module, kind_name, at)?;
                if kind.what != What::Kind {
                    return err(at, format!("'{}{}{}' is a {}, not a kind, so it has no variant '{}'", module_name, MODULE_SEPARATOR, kind_name, kind.what.word(), variant));
                }
                self.variant(&kind, &format!("{}{}{}", module_name, MODULE_SEPARATOR, kind_name), variant, at).map(Some)
            }
            [first, ..] if !self.modules.contains_key(*first) => {
                err(at, format!("'{}' names nothing: '{}' isn't a module this file includes{}", written, first, self.unnamed_hint(written, first, calls)))
            }
            _ => err(at, format!("'{}' names nothing: a variant of a module is named with three names, as in 'geometry.Figure.Square'", written)),
        }
    }

    /// The end of an error for `name`, written where a kind belongs, when a
    /// module this file includes by name has a kind of that name.
    fn kind_hint(&self, name: &str) -> String {
        let mut locals: Vec<&String> = self.modules.keys().collect();
        locals.sort();
        locals
            .into_iter()
            .find(|local| {
                self.modules.get(*local).is_some_and(|module| {
                    !module.hidden.contains_key(name) && module.names.get(name).is_some_and(|entry| entry.what == What::Kind && !entry.private)
                })
            })
            .map_or_else(String::new, |local| {
                format!("; module '{0}' has one: write '{0}{1}{2}', or bring it in with 'include … ({2})'", local, MODULE_SEPARATOR, name)
            })
    }

    /// The end of a "names nothing" error for `written`, whose first name
    /// `first` this file doesn't give a module or kind: how the module with that
    /// file name is included, if it is, or for a call, the change to a slice it
    /// may mean.
    fn unnamed_hint(&self, written: &str, first: &str, calls: bool) -> String {
        match self.unnamed.iter().find(|(file_name, _, _)| file_name == first) {
            Some((_, Some(alias), path)) => format!("; module '{}' is included as '{}'", path, alias),
            Some((_, None, path)) => format!("; module '{}' is included with only its parts, which are named alone", path),
            None if calls => match unqualified(written) {
                "push" | "append" | "insert" => "; a slice adds an item with 'add', as in 'xs.add(v)' or 'xs.add(i, v)'".to_string(),
                "pop" | "remove" | "delete" => "; a slice takes an item out with 'del', as in 'xs.del(i)' or 'xs.del()'".to_string(),
                _ => String::new(),
            },
            None => String::new(),
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
            None if tag == TYPE_WORD => {
                err(at, format!("'type' is read on a value, as in 'k.type', and '{}' is a kind, whose type is its name", kind_name))
            }
            None => err(at, format!("kind '{}' has no variant '{}'", kind_name, tag)),
        }
    }

    /// The kind `name` (as written, maybe `m.Kind`) stands for, if it names one.
    fn kind(&self, name: &str, at: Span) -> Result<Option<Entry>> {
        if !name.contains(MODULE_SEPARATOR) || name.split(MODULE_SEPARATOR).count() == 2 && self.modules.contains_key(module_of(name)) {
            return Ok(self.lookup(name, at, false)?.filter(|entry| entry.what == What::Kind));
        }
        Ok(None)
    }

    /// `name` rewritten, when it stands for a declaration of one of the kinds `whats`.
    fn rename(&self, name: &mut String, at: Span, whats: &[What]) -> Result<()> {
        if let Some(entry) = self.lookup(name, at, whats.contains(&What::Task))?
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
                None => match name.split_once(MODULE_SEPARATOR) {
                    Some((first, _)) if !self.modules.contains_key(first) => {
                        return err(*at, format!("'{}' names nothing: '{}' isn't a module this file includes{}", name, first, self.unnamed_hint(name, first, false)));
                    }
                    _ => return err(*at, format!("'{}' isn't a kind, so a pattern can't name a variant of it{}", name, self.kind_hint(name))),
                },
            }
        }
        Ok(())
    }

    /// `Light.Red(…)` or `m.f(…)`, a variant or a module's task read as a method
    /// called on a value, rewritten to the call it is; whether `expr` was one. A
    /// name before the dot is a module this file includes or a kind, and
    /// anything else, or anything longer, is a value with a method. (A function of its own: `expr`'s
    /// frame is on the stack once per nesting level.)
    fn call_as_name(&self, expr: &mut Expr) -> Result<bool> {
        let ExprKind::MethodCall { dotted: Some(dotted), .. } = &expr.kind else { return Ok(false) };
        let parts: Vec<&str> = dotted.split(MODULE_SEPARATOR).collect();
        // `X.new(…)`: a shared method, called on the shape or kind itself,
        // whose task is named like any method's (see `method_name`).
        if let Some(name) = self.shared_method(&parts, expr.span)? {
            let ExprKind::MethodCall { args, .. } = &mut expr.kind else { return Ok(false) };
            let args = std::mem::take(args);
            expr.kind = ExprKind::Call { name, types: Vec::new(), args };
            return Ok(true);
        }
        // A name is `Kind.Variant`, `m.f`, or `m.Kind.Variant`; more after it
        // is a method called on the variant, as in `Suit.Spades.name()`.
        let named = match parts.as_slice() {
            [module, _] if self.modules.contains_key(*module) => true,
            // `m.Blue.str()` is a method called on the variant `m.Blue`.
            [module, second, _] if self.modules.contains_key(*module) => {
                !self.modules.get(*module).and_then(|names| names.names.get(*second)).is_some_and(|entry| entry.what == What::Variant)
            }
            [kind, _] => self.kind(kind, expr.span)?.is_some(),
            _ => false,
        };
        if !named {
            return Ok(false);
        }
        let mut name = dotted.clone();
        self.rename(&mut name, expr.span, &[What::Task, What::Variant])?;
        let ExprKind::MethodCall { args, .. } = &mut expr.kind else { return Ok(false) };
        let args = std::mem::take(args);
        expr.kind = ExprKind::Call { name, types: Vec::new(), args };
        Ok(true)
    }

    /// The task a call written `X.m(…)`, `Kind.m(…)`, or `m.X.m(…)` (as
    /// `parts`) calls: the method `m` of the shape or kind, which the checker
    /// requires to be shared. A kind's variant is no method, so `Kind.Tag(…)`
    /// is none. No variable is named like a shape or a kind, so the name is
    /// the type's.
    fn shared_method(&self, parts: &[&str], at: Span) -> Result<Option<String>> {
        let (owner, method) = match parts {
            [owner, method] if !self.modules.contains_key(*owner) => (owner.to_string(), *method),
            [module, owner, method] if self.modules.contains_key(*module) => (format!("{}{}{}", module, MODULE_SEPARATOR, owner), *method),
            _ => return Ok(None),
        };
        let Some(entry) = self.lookup(&owner, at, false)? else { return Ok(None) };
        let variant = self.variants.get(&entry.full).is_some_and(|tags| tags.contains_key(method));
        match entry.what {
            What::Shape => Ok(Some(method_name(&entry.full, method))),
            What::Kind if !variant => Ok(Some(method_name(&entry.full, method))),
            _ => Ok(None),
        }
    }

    /// `Suit.Hearts` (or `m.Suit.Hearts`), a variant read as a field of a
    /// name, rewritten to the variant, or `Point.made`, a shared field of a
    /// shape, rewritten to `ExprKind::Shared`; whether `expr` was one. No
    /// variable can be named like a kind or a shape (see `Checker::define`).
    /// (A function of its own:
    /// `expr`'s frame is on the stack once per nesting level.)
    fn variant_as_field(&self, expr: &mut Expr) -> Result<bool> {
        let ExprKind::Field(base, tag) = &expr.kind else { return Ok(false) };
        let ExprKind::Var(name) = &base.kind else { return Ok(false) };
        if let Some(kind) = self.kind(name, base.span)? {
            let variant = self.variant(&kind, name, tag, expr.span)?;
            // Where the kind is written, as for `Kind.Variant(…)`.
            expr.span = base.span;
            expr.kind = ExprKind::Var(variant.full);
            return Ok(true);
        }
        // `Shape.f` (or `m.Shape.f`): a shared field of the shape. No variable
        // is named like a shape either, so the name is the shape's.
        let entry = match name.contains(MODULE_SEPARATOR) {
            true => self.lookup(name, base.span, false)?,
            false => self.lookup(name, base.span, false)?.filter(|entry| entry.what == What::Shape),
        };
        if let Some(entry) = entry {
            if entry.what == What::Shape {
                let field = tag.clone();
                // Where the field's name is written, as for a field of a value.
                expr.kind = ExprKind::Shared { shape: entry.full, field };
                return Ok(true);
            }
            // `m.Name.x`, where `m.Name` is a task. (A variable may be named
            // like a task of this file, so `Name.x` may be a field.)
            return err(expr.span, format!("'{}' is a {}, not a kind, so it has no variant '{}'", name, entry.what.word(), tag));
        }
        Ok(false)
    }

    /// A written type, at `span`, with its shape and kind names rewritten.
    fn ty(&self, ty: &Type, span: Span) -> Result<Type> {
        Ok(match ty {
            // A type the declaration takes, not a shape or kind (see
            // `enter_type_params`).
            Type::Named(name, _) if self.params.contains(name) => ty.clone(),
            Type::Applied(applied) => {
                let at = self
                    .type_places
                    .get(&span.place_key())
                    .and_then(|parts| parts.iter().find(|(part, _)| *part == applied.name))
                    .map_or(span, |(_, at)| *at);
                let mut name = applied.name.clone();
                self.rename(&mut name, at, &[What::Shape, What::Kind])?;
                let types = applied.types.iter().map(|ty| self.ty(ty, span)).collect::<Result<_>>()?;
                Type::Applied(Box::new(Applied { name, types, region: applied.region.clone() }))
            }
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
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| self.ty(t, span)).collect::<Result<_>>()?),
            other => other.clone(),
        })
    }

    fn block(&self, block: &mut Block) -> Result<()> {
        block.iter_mut().try_for_each(|stmt| self.stmt(stmt))
    }

    /// A name a binding declares, with its written type, or the names of a tuple's items.
    fn bound(&self, bound: &mut Bound) -> Result<()> {
        if let (Some(ty), Some(at)) = (&bound.ty, bound.ty_span) {
            bound.ty = Some(self.ty(ty, at)?);
        }
        bound.parts.iter_mut().try_for_each(|part| self.bound(part))
    }

    /// A place an assignment writes to, or the places of a tuple's items.
    fn target(&self, target: &mut Target) -> Result<()> {
        match target {
            Target::Discard(_) => Ok(()),
            Target::Place(e) => self.expr(e),
            Target::Tuple(parts, _) => parts.iter_mut().try_for_each(|part| self.target(part)),
        }
    }

    fn stmt(&self, stmt: &mut Stmt) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Binding { names, init, .. } => {
                names.iter_mut().try_for_each(|bound| self.bound(bound))?;
                init.iter_mut().try_for_each(|e| self.expr(e))
            }
            StmtKind::Assign(targets, values) => {
                targets.iter_mut().try_for_each(|target| self.target(target))?;
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
            StmtKind::Block(body) => self.block(body),
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
            StmtKind::Region { .. } => Ok(()),
        }
    }

    fn expr(&self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        if matches!(&expr.kind, ExprKind::Field(base, _) if matches!(base.kind, ExprKind::Var(_))) && self.variant_as_field(expr)? {
            return Ok(());
        }
        if matches!(expr.kind, ExprKind::Edit { .. } | ExprKind::Copy { region: None, .. }) && self.edit_as_variant(expr)? {
            return Ok(());
        }
        // A name of the program is written once; looking the result up again, as
        // if the file had written it, would not find it.
        if matches!(expr.kind, ExprKind::MethodCall { .. }) && self.call_as_name(expr)? {
            let ExprKind::Call { args, .. } = &mut expr.kind else { return Ok(()) };
            return args.iter_mut().try_for_each(|arg| self.expr(arg));
        }
        match &mut expr.kind {
            // A region is a name of the task, never of the program.
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::RegionAfter(_) => Ok(()),
            // Only the resolver makes one, above.
            ExprKind::Shared { .. } | ExprKind::TypeOf(_) => Ok(()),
            // A variable keeps its name. No variable can be named like a variant
            // (see `Checker::define`), so a variant written as a value is renamed.
            ExprKind::Var(name) => self.rename(name, span, &[What::Variant]),
            ExprKind::Call { name, types, args } => {
                self.rename(name, span, &[What::Task, What::Variant])?;
                for (ty, at) in types.iter_mut() {
                    *ty = self.ty(&ty.clone(), *at)?;
                }
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            // A method is named by the type of the value it is called on, which
            // only the checker knows, so nothing here is renamed.
            ExprKind::MethodCall { value, args, .. } => {
                self.expr(value)?;
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            ExprKind::StructInit { name, types, fields } => {
                self.rename(name, span, &[What::Shape, What::Kind])?;
                // The types a literal names are written types too.
                for (ty, at) in types.iter_mut() {
                    *ty = self.ty(&ty.clone(), *at)?;
                }
                fields.iter_mut().try_for_each(|field| self.expr(&mut field.value))
            }
            ExprKind::Construct { args, .. } => args.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Field(base, _) | ExprKind::Deref(base) | ExprKind::Ref(base) | ExprKind::Unary(_, base) | ExprKind::Fail(base) | ExprKind::Wrap(base) => self.expr(base),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Array(values) | ExprKind::Tuple(values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::New { ty, ty_span, value, .. } => {
                **ty = self.ty(ty, *ty_span)?;
                self.expr(value)
            }
            ExprKind::PlanB { value, body, .. } => {
                self.expr(value)?;
                self.block(body)
            }
            // The same statement a `match` or a `when` on its own is.
            ExprKind::Choice(stmt) => self.stmt(stmt),
            ExprKind::Edit { items, args, .. } => {
                self.expr(items)?;
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            ExprKind::Scan { args, .. } | ExprKind::Join { parts: args, .. } => args.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Copy { value, .. } | ExprKind::ToText { value, .. } => self.expr(value),
        }
    }

    /// `Kind.add(…)`, `Kind.del(…)`, or `Kind.copy()`, which the parser reads as
    /// a change to a slice or a copy, rewritten to the call of the kind's
    /// variant; whether `expr` was one. No variable can be named like a kind
    /// (see `Checker::define`). (A function of its own: `expr`'s frame is on the
    /// stack once per nesting level.)
    fn edit_as_variant(&self, expr: &mut Expr) -> Result<bool> {
        let (items, word) = match &expr.kind {
            ExprKind::Edit { items, op, .. } => (items, op.word()),
            ExprKind::Copy { value, region: None, .. } => (value, COPY),
            _ => return Ok(false),
        };
        let ExprKind::Var(name) = &items.kind else { return Ok(false) };
        if self.kind(name, items.span)?.is_none() {
            return Ok(false);
        }
        let written = format!("{}{}{}", name, MODULE_SEPARATOR, word);
        // Where the kind is written, as for any `Kind.Variant(…)`.
        expr.span = items.span;
        let args = match &mut expr.kind {
            ExprKind::Edit { args, .. } => std::mem::take(args),
            _ => Vec::new(),
        };
        expr.kind = ExprKind::Call { name: written, types: Vec::new(), args };
        self.expr(expr)?;
        Ok(true)
    }
}

fn err<T>(span: Span, message: impl Into<String>) -> Result<T> {
    Err(Error::at(span, message))
}
