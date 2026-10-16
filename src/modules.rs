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

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::Lexer;
use crate::library;
use crate::parser::{MAX_NESTING, Parser, needs_list, receiver};

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
    /// Every pattern of every module, its names as the program names them
    /// (see `PatternDef`).
    pub patterns: Vec<PatternDef>,
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

/// What one walk of a module resolves (see `State::resolve`).
#[derive(Clone, Copy)]
enum Pass<'a> {
    /// The names in the types of its type aliases, which make the program's
    /// aliases.
    Aliases,
    /// The kinds each of its kinds imitates (see `KindDef::imitates`), with the
    /// program's aliases put in.
    Imitated(&'a TypeAliases),
    /// The names in its items, with the program's aliases put in.
    Items(&'a TypeAliases),
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
    /// Its type aliases, which name types and are no items (see `TypeAliases`).
    type_aliases: Vec<AliasDef>,
    /// Its patterns, which are no items either (see `PatternDef`).
    patterns: Vec<PatternDef>,
    /// Whether it has a loop that gives a value (see `Parser::loop_values`).
    loop_values: bool,
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
        // A loop that gives a value fails with `std/loops`'s `LoopResult`, which
        // a file names only if it includes it.
        let key = PathBuf::from(format!("<{0}>/{0}/{1}", library::ROOT, library::LOOPS));
        if self.modules.iter().any(|module| module.loop_values)
            && !self.by_path.contains_key(&key)
            && let Some(source) = library::module(library::LOOPS)
        {
            let id = format!("{}/{}", library::ROOT, library::LOOPS);
            let path = PathBuf::from(format!("{}.vela", id));
            let loops = self.parse(id, &path, key, source.to_string(), None)?;
            self.visit(loops, loader)?;
        }
        let tables: Vec<Names> = self.modules.iter().map(own_names).collect();
        let variants = variants_of_kinds(&self.modules);
        // Every name in the program, by the identity of its module.
        let mut by_module: HashMap<&str, Vec<&str>> = HashMap::new();
        for entry in tables.iter().flat_map(HashMap::values) {
            if let Some((id, _)) = entry.full.rsplit_once(MODULE_SEPARATOR) {
                by_module.entry(id).or_default().push(&entry.full);
            }
        }
        // The types of the type aliases first, with the names in them written
        // as the program names them, then the kinds each kind imitates, then
        // the items, with each alias put in.
        for index in 0..self.modules.len() {
            self.resolve(index, &tables, &variants, &by_module, Pass::Aliases)?;
        }
        let aliases: TypeAliases = self.modules.iter().flat_map(|module| &module.type_aliases).map(|alias| (alias.name.clone(), alias.ty.clone())).collect();
        // One that stands for itself, through others or not, is told where it
        // is declared, even when nothing names it.
        for alias in self.modules.iter().flat_map(|module| &module.type_aliases) {
            expand_aliases(&Type::Named(alias.name.clone(), None), &aliases, &mut Vec::new(), &mut Vec::new(), &|_| alias.ty_span)?;
        }
        for index in 0..self.modules.len() {
            self.resolve(index, &tables, &variants, &by_module, Pass::Imitated(&aliases))?;
        }
        // `Kind.Variant` names a variant a kind has from one it imitates too.
        let mut variants = variants;
        imitated_variants(&self.modules, &mut variants)?;
        let mut scopes = Vec::new();
        for index in 0..self.modules.len() {
            scopes.push(self.resolve(index, &tables, &variants, &by_module, Pass::Items(&aliases))?);
        }
        imitate(&mut self.modules);
        follow_patterns(&mut self.modules);
        let patterns = self.modules.iter().flat_map(|module| module.patterns.iter().cloned()).collect();
        let mut items = Vec::new();
        for index in std::mem::take(&mut self.order) {
            if let Some(module) = self.modules.get_mut(index) {
                items.append(&mut module.items);
            }
        }
        Ok(Program { items, modules: scopes, type_places: std::mem::take(&mut self.type_places), patterns })
    }

    /// Lexes and parses `source` as the next file, the module with identity
    /// `id`, read from `path`, which `key` tells from every other file, loaded by
    /// the `include` at `included_at`.
    fn parse(&mut self, id: String, path: &Path, key: PathBuf, source: String, included_at: Option<Span>) -> Result<usize> {
        let file = self.files.len();
        self.files.push(SourceFile { path: path.to_path_buf(), source, included_at });
        let source = self.files.get(file).map_or("", |f| f.source.as_str());
        let (tokens, spans, ends) = Lexer::for_file(source, file).tokenize()?;
        let mut parser = Parser::new(tokens, spans, ends);
        let items = parser.parse_file()?;
        check_natives(&id, &items)?;
        self.type_places.extend(parser.type_places());
        let includes = parser.includes();
        let type_aliases = parser.type_aliases();
        let patterns = parser.patterns();
        let loop_values = parser.loop_values();
        let index = self.modules.len();
        self.by_path.insert(key, index);
        self.modules.push(Module { id, file, includes, included: Vec::new(), items, type_aliases, patterns, loop_values });
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
        // The includes seen so far, by the file each names and by the name it
        // is known by, to find one included twice.
        let mut files: HashMap<PathBuf, Span> = HashMap::new();
        let mut locals: HashMap<&str, Span> = HashMap::new();
        for include in &includes {
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
            if let Some(at) = files.get(&key) {
                return err(include.name_span, format!("this module is already included at {}:{}; a file includes each module once", at.line, at.col));
            }
            files.insert(key.clone(), include.name_span);
            if let Some(local) = include.local_name()
                && let Some(&at) = locals.get(local)
            {
                return err(
                    include.local_span(),
                    format!(
                        "another module named '{}' is already included at {}:{}; a file can't include two modules with one name, so give one of them another name with '=> name'",
                        local, at.line, at.col
                    ),
                );
            }
            if let Some(local) = include.local_name() {
                locals.insert(local, include.local_span());
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

    /// Resolves the names in module `index` that `pass` reads, and returns what
    /// the checker needs to know about it.
    fn resolve(&mut self, index: usize, tables: &[Names], variants: &Variants, by_module: &HashMap<&str, Vec<&str>>, pass: Pass) -> Result<ModuleScope> {
        let Some(module) = self.modules.get(index) else { return Ok(ModuleScope::default()) };
        let mut own = tables.get(index).cloned().unwrap_or_default();
        // The builtin kinds are written like declared ones, in every file: a task
        // may send one (`fail ParseError.TooLarge`) as well as read one. A file
        // declaring such a name keeps its own entry here, so that the checker
        // reports the clash rather than this (see `check::collect_declarations`).
        for kind in builtin_kinds() {
            own.entry(kind.name.clone()).or_insert_with(|| builtin_kind_entry(&kind.name, What::Kind, None));
        }
        let none = TypeAliases::new();
        let aliases = match pass {
            Pass::Aliases => None,
            Pass::Imitated(aliases) | Pass::Items(aliases) => Some(aliases),
        };
        let mut resolver = Resolver {
            own,
            modules: HashMap::new(),
            imports: HashMap::new(),
            unnamed: Vec::new(),
            variants,
            tables,
            type_places: &self.type_places,
            aliases: aliases.unwrap_or(&none),
            params: Vec::new(),
            spelling: Spelling::default(),
            this_names: Vec::new(),
        };
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
                if let Some(names) = tables.get(target) {
                    resolver.modules.insert(local.to_string(), ModuleNames { names, hidden });
                }
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
                    return err(part.span, format!("module '{}' has no task, shape, kind, type alias, variant, or macro named '{}'", label, part.name));
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
        let spelling = spelling(module, tables.get(index), tables, variants, by_module, &imports, &includes);
        resolver.spelling = spelling.clone();
        let scope = ModuleScope { name: module.id.clone(), file: module.file, includes, imports, spelling };
        let Some(module) = self.modules.get_mut(index) else { return Ok(scope) };
        match pass {
            Pass::Aliases => {
                for alias in &mut module.type_aliases {
                    resolver.type_alias(alias)?;
                }
                return Ok(scope);
            }
            Pass::Imitated(_) => {
                for item in &mut module.items {
                    match item {
                        Item::Kind(k) => resolver.imitated(k)?,
                        Item::Shape(s) => resolver.followed(s)?,
                        Item::Task(_) | Item::Macro(_) => {}
                    }
                }
                return Ok(scope);
            }
            Pass::Items(_) => {
                for pattern in &mut module.patterns {
                    resolver.pattern_def(pattern)?;
                }
            }
        }
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
fn spelling(
    module: &Module,
    own: Option<&Names>,
    tables: &[Names],
    variants: &Variants,
    by_module: &HashMap<&str, Vec<&str>>,
    imports: &[(String, String, Span)],
    includes: &[IncludeScope],
) -> Spelling {
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
    // What a loop that gives a value fails with is named by its own name
    // where the file brings in no way to write it.
    names.entry(library::LOOP_RESULT.to_string()).or_insert_with(|| unqualified(library::LOOP_RESULT).to_string());
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
    let taken: HashSet<&str> = includes.iter().filter_map(|include| include.local.as_deref().filter(|local| *local != include.id)).collect();
    for full in taken.iter().filter_map(|id| by_module.get(id)).flatten() {
        names.entry(full.to_string()).or_insert_with(|| format!("./{}", full));
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
    Alias,
    Pattern,
    Variant,
    Macro,
}

impl What {
    fn word(self) -> &'static str {
        match self {
            What::Task => "task",
            What::Shape => "shape",
            What::Kind => "kind",
            What::Alias => "type alias",
            What::Pattern => "pattern",
            What::Variant => "variant",
            What::Macro => "macro",
        }
    }
}

/// A task, shape, kind, type alias, variant, or macro declared in a module.
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
            Item::Macro(m) => add(&m.name, What::Macro, m.private, m.span, None),
        }
    }
    for alias in &module.type_aliases {
        add(&alias.name, What::Alias, alias.private, alias.span, None);
    }
    for pattern in &module.patterns {
        add(&pattern.name, What::Pattern, pattern.private, pattern.span, None);
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

/// Adds to `variants` each variant a kind has from the kinds it imitates,
/// named through it (`AppError.Empty`; see `imitated_variant`), once it is
/// known that no kind imitates itself, through others or not, that each kind
/// imitated is given the types it takes, and that no two variants of a kind
/// share a name.
fn imitated_variants(modules: &[Module], variants: &mut Variants) -> Result<()> {
    let builtin = builtin_kinds();
    let mut kinds: HashMap<String, &KindDef> = builtin.iter().map(|kind| (kind.name.clone(), kind)).collect();
    let mut order = Vec::new();
    for module in modules {
        for item in &module.items {
            if let Item::Kind(k) = item {
                let full = if module.id.is_empty() { k.name.clone() } else { format!("{}{}{}", module.id, MODULE_SEPARATOR, k.name) };
                kinds.entry(full.clone()).or_insert(k);
                order.push(full);
            }
        }
    }
    let mut done = HashMap::new();
    for full in &order {
        let tags = kind_tags(full, &kinds, &mut done, &mut Vec::new())?;
        let Some(kind) = kinds.get(full.as_str()) else { continue };
        let entries = variants.entry(full.clone()).or_default();
        for (tag, _, at) in tags.iter().take(tags.len().saturating_sub(kind.variants.len())) {
            let entry = Entry { full: imitated_variant(full, tag), what: What::Variant, private: kind.private, span: *at, kind: Some(full.clone()) };
            entries.entry(tag.clone()).or_insert(entry);
        }
    }
    Ok(())
}

/// The names of the variants of kind `full` (its name in the program), those
/// it has from the kinds it imitates first, each with the kind it is declared
/// in and where this kind has it from (its own position, for its own). `done`
/// keeps the kinds already read, and `through` the kinds being read, to find
/// one that imitates itself.
fn kind_tags(full: &str, kinds: &HashMap<String, &KindDef>, done: &mut HashMap<String, Vec<(String, String, Span)>>, through: &mut Vec<String>) -> Result<Vec<(String, String, Span)>> {
    if let Some(tags) = done.get(full) {
        return Ok(tags.clone());
    }
    let Some(kind) = kinds.get(full) else { return Ok(Vec::new()) };
    let mut tags: Vec<(String, String, Span)> = Vec::new();
    through.push(full.to_string());
    for (ty, at) in &kind.imitates {
        let (source, given) = match ty {
            Type::Named(name, _) => (name, 0),
            Type::Applied(applied) => (&applied.name, applied.types.len()),
            _ => continue,
        };
        if source == full {
            return err(*at, format!("kind '{}' can't imitate itself", unqualified(full)));
        }
        if let Some(start) = through.iter().position(|name| name == source) {
            let mut circle: Vec<&str> = through.iter().skip(start).map(|name| unqualified(name)).collect();
            circle.push(unqualified(source));
            return err(*at, format!("kinds can't imitate one another in a circle: {}", circle.join(" -> ")));
        }
        if let Some((_, first)) = kind.imitates.iter().take_while(|(_, before)| before != at).find(|(before, _)| base_name(before) == Some(source)) {
            return err(*at, format!("'{}' is imitated already, at {}:{}; a kind imitates each kind once", unqualified(source), first.line, first.col));
        }
        let Some(imitated) = kinds.get(source.as_str()) else { continue };
        let takes = imitated.types.len();
        if given != takes {
            let names: Vec<&str> = imitated.types.iter().map(|param| param.name.as_str()).collect();
            let message = match (takes, given) {
                (0, _) => format!("kind '{}' takes no types; leave out '<…>'", unqualified(source)),
                (1, _) => format!("kind '{0}' takes a type, so name it, as in 'kind {1}::{0}<int>' or 'kind {1}<{2}>::{0}<{2}>'", unqualified(source), unqualified(full), names.join(", ")),
                _ => format!("kind '{0}' takes {1} types, so name them all, as in 'kind {2}<{3}>::{0}<{3}>'", unqualified(source), takes, unqualified(full), names.join(", ")),
            };
            return err(*at, message);
        }
        for (tag, from, _) in kind_tags(source, kinds, done, through)? {
            if let Some((_, other, _)) = tags.iter().find(|(seen, _, _)| *seen == tag) {
                return err(
                    *at,
                    format!("'{}' is a variant of '{}', and of '{}' too, which '{}' also imitates; each variant of a kind has a name of its own", tag, unqualified(&from), unqualified(other), unqualified(full)),
                );
            }
            tags.push((tag, from, *at));
        }
    }
    through.pop();
    for variant in &kind.variants {
        if let Some((_, from, at)) = tags.iter().find(|(tag, _, _)| *tag == variant.tag) {
            return err(
                variant.span,
                format!("'{}' is already a variant of '{}', which has it from '{}', imitated at {}:{}", variant.tag, unqualified(full), unqualified(from), at.line, at.col),
            );
        }
        tags.push((variant.tag.clone(), full.to_string(), variant.span));
    }
    done.insert(full.to_string(), tags.clone());
    Ok(tags)
}

/// The kind a kind imitates names, `Option` in `Option<int>`.
fn base_name(ty: &Type) -> Option<&String> {
    match ty {
        Type::Named(name, _) => Some(name),
        Type::Applied(applied) => Some(&applied.name),
        _ => None,
    }
}

/// Gives each shape and kind that follows patterns the default methods it
/// doesn't write itself, each a copy of the pattern's called on a value of the
/// follower (its header and body name the type following the pattern as
/// `THIS_TYPE`, which the checker reads as the follower), and adds to
/// each pattern's module, for each default method, the generic task it is
/// checked as once (see `pattern_default`).
fn follow_patterns(modules: &mut [Module]) {
    let patterns: HashMap<String, PatternDef> = modules.iter().flat_map(|module| &module.patterns).map(|pattern| (pattern.name.clone(), pattern.clone())).collect();
    for module in modules.iter_mut() {
        let written: HashSet<String> = module.items.iter().filter_map(|item| if let Item::Task(t) = item && t.owner.is_some() { Some(t.name.clone()) } else { None }).collect();
        let mut added: Vec<TaskDef> = Vec::new();
        for item in &module.items {
            let (owner, types, follows) = match item {
                Item::Shape(s) => (&s.name, &s.types, &s.follows),
                Item::Kind(k) => (&k.name, &k.types, &k.follows),
                Item::Task(_) | Item::Macro(_) => continue,
            };
            for pattern in follows.iter().filter_map(|(ty, _)| if let Type::Named(name, _) = ty { patterns.get(name) } else { None }) {
                for method in pattern.methods.iter().filter(|method| !method.body.is_empty()) {
                    let Some(own) = method.owner.as_ref().map(|owner| owner.name.as_str()) else { continue };
                    let full = method_name(owner, own);
                    if written.contains(&full) || added.iter().any(|task| task.name == full) {
                        continue;
                    }
                    let mut copy = method.clone();
                    copy.name = full;
                    copy.types = types.clone();
                    if let Some(first) = copy.params.first_mut() {
                        *first = receiver(owner, types, method.span);
                    }
                    added.push(copy);
                }
            }
        }
        module.items.extend(added.into_iter().map(Item::Task));
        for pattern in &module.patterns {
            for method in pattern.methods.iter().filter(|method| !method.body.is_empty()) {
                let Some(own) = method.owner.as_ref().map(|owner| owner.name.as_str()) else { continue };
                let mut task = method.clone();
                task.name = pattern_default(&pattern.name, own);
                task.types = vec![TypeParam { name: THIS_TYPE.to_string(), span: pattern.span, needs: Vec::new(), patterns: vec![(pattern.name.clone(), pattern.span)], group: false }];
                module.items.push(Item::Task(task));
            }
        }
    }
}

/// Puts in each kind that imitates others the variants it has from them,
/// before its own (see `KindDef::imitated`): each carries what it carries in
/// the kind it comes from, with the types given that kind in place of the
/// ones it takes, and is written where that kind is named. Every name in the
/// program is resolved by now, so a variant carries the same types wherever
/// it is put.
fn imitate(modules: &mut [Module]) {
    let mut kinds: HashMap<String, KindDef> = builtin_kinds().into_iter().map(|kind| (kind.name.clone(), kind)).collect();
    for item in modules.iter().flat_map(|module| &module.items) {
        if let Item::Kind(k) = item {
            kinds.entry(k.name.clone()).or_insert_with(|| k.clone());
        }
    }
    let mut done = HashMap::new();
    for item in modules.iter_mut().flat_map(|module| &mut module.items) {
        if let Item::Kind(k) = item
            && !k.imitates.is_empty()
        {
            let mut variants = imitated_of(&k.name, &kinds, &mut done);
            k.imitated = variants.len();
            variants.append(&mut k.variants);
            k.variants = variants;
        }
    }
}

/// The variants kind `name` has from the kinds it imitates (see `imitate`).
fn imitated_of(name: &str, kinds: &HashMap<String, KindDef>, done: &mut HashMap<String, Vec<Variant>>) -> Vec<Variant> {
    let Some(kind) = kinds.get(name) else { return Vec::new() };
    let mut variants = Vec::new();
    for (ty, at) in &kind.imitates {
        let (source, given) = match ty {
            Type::Named(name, _) => (name, &[][..]),
            Type::Applied(applied) => (&applied.name, applied.types.as_slice()),
            _ => continue,
        };
        let Some(imitated) = kinds.get(source) else { continue };
        let types: HashMap<&str, &Type> = imitated.types.iter().map(|param| param.name.as_str()).zip(given).collect();
        let all = match done.get(source) {
            Some(all) => all.clone(),
            None => {
                let mut all = imitated_of(source, kinds, done);
                all.extend(imitated.variants.iter().cloned());
                done.insert(source.clone(), all.clone());
                all
            }
        };
        for variant in all {
            let payload = variant.payload.iter().map(|field| Field { ty: field.ty.replace_names(&|name| types.get(name).map(|ty| (*ty).clone())), span: *at, ty_span: *at, ..field.clone() }).collect();
            variants.push(Variant { tag: variant.tag, payload, span: *at });
        }
    }
    variants
}

/// The type each type alias of the program stands for, by the alias's name in
/// the program, with the names in it written as the program names them and
/// the aliases in it left as they are (see `expand_aliases`).
type TypeAliases = HashMap<String, Type>;

/// `ty` with each type alias in it put in its place, and each alias in that
/// alias's type, as if the type it stands for were written there: `Names@r`,
/// with `alias Names => [text]`, is `[text]@r`. `stack` holds the aliases being
/// put in, to find one that stands for itself, and `at` where an alias of the
/// program is written, for errors. An alias written with a region, of a type
/// that lives in none, adds the region, the alias, and its type to `unplaced`
/// (see `Resolver::ty`).
fn expand_aliases(ty: &Type, aliases: &TypeAliases, stack: &mut Vec<String>, unplaced: &mut Vec<(String, String, Type)>, at: &dyn Fn(&str) -> Span) -> Result<Type> {
    let each = |t: &Type, stack: &mut Vec<String>, unplaced: &mut Vec<(String, String, Type)>| expand_aliases(t, aliases, stack, unplaced, at);
    let task = |task: &TaskType, stack: &mut Vec<String>, unplaced: &mut Vec<(String, String, Type)>| -> Result<TaskType> {
        let params = task.params.iter().map(|t| expand_aliases(t, aliases, stack, unplaced, at)).collect::<Result<_>>()?;
        Ok(TaskType { params, taken: task.taken.clone(), ret: expand_aliases(&task.ret, aliases, stack, unplaced, at)? })
    };
    Ok(match ty {
        Type::Named(name, region) => {
            let Some(aliased) = aliases.get(name) else { return Ok(ty.clone()) };
            if let Some(start) = stack.iter().position(|alias| alias == name) {
                let cycle: Vec<&str> = stack.iter().skip(start).chain([name]).map(|alias| unqualified(alias)).collect();
                return err(at(name), format!("type aliases can't stand for one another in a circle: {}", cycle.join(" -> ")));
            }
            stack.push(name.clone());
            let expanded = expand_aliases(aliased, aliases, stack, unplaced, at)?;
            stack.pop();
            let Some(region) = region else { return Ok(expanded) };
            if expanded == Type::Region {
                return err(at(name), "a region doesn't live in another region");
            }
            // What has no region takes it, as `parse_type_region` gives one.
            let mut named = !expanded.regions().is_empty();
            let placed = expanded.map_regions_of_names(&mut |_| {
                named = true;
                Some(region.clone())
            });
            if !named {
                unplaced.push((region.clone(), name.clone(), placed.clone()));
            }
            placed
        }
        Type::Applied(applied) => {
            let types = applied.types.iter().map(|t| each(t, stack, unplaced)).collect::<Result<_>>()?;
            Type::Applied(Box::new(Applied { name: applied.name.clone(), types, region: applied.region.clone() }))
        }
        Type::Ptr(inner, region) => Type::Ptr(Box::new(each(inner, stack, unplaced)?), region.clone()),
        Type::Slice(inner, region) => Type::Slice(Box::new(each(inner, stack, unplaced)?), region.clone()),
        Type::Map(key, value, region) => Type::Map(Box::new(each(key, stack, unplaced)?), Box::new(each(value, stack, unplaced)?), region.clone()),
        Type::Array(inner, count) => Type::Array(Box::new(each(inner, stack, unplaced)?), *count),
        Type::Range(inner) => Type::Range(Box::new(each(inner, stack, unplaced)?)),
        Type::Maybe(inner) => Type::Maybe(Box::new(each(inner, stack, unplaced)?)),
        Type::Fail(inner) => Type::Fail(Box::new(each(inner, stack, unplaced)?)),
        Type::Or(value, error) => Type::Or(Box::new(each(value, stack, unplaced)?), Box::new(each(error, stack, unplaced)?)),
        Type::Results(types) => Type::Results(types.iter().map(|t| each(t, stack, unplaced)).collect::<Result<_>>()?),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|t| each(t, stack, unplaced)).collect::<Result<_>>()?),
        Type::Task(t) => Type::Task(Box::new(task(t, stack, unplaced)?)),
        Type::Closure(closure) => Type::Closure(Box::new(ClosureType { task: task(&closure.task, stack, unplaced)?, region: closure.region.clone(), mode: closure.mode })),
        other => other.clone(),
    })
}

/// What a file can name of a module it includes under a name.
struct ModuleNames<'a> {
    names: &'a Names,
    /// The parts brought in under another name, which the module's name can't
    /// reach: each part's name, with its alias and where that is written.
    hidden: HashMap<String, Binding>,
}

/// Rewrites the names written in one module to their names in the program.
struct Resolver<'a> {
    own: Names,
    /// The modules this one includes, by the name it names each by.
    modules: HashMap<String, ModuleNames<'a>>,
    /// The names brought in by name, by the name this file writes (an alias if
    /// there is one), with where each is brought in.
    imports: HashMap<String, (Entry, Span)>,
    /// The modules this one includes that its path's last part doesn't name:
    /// that part, the name it is included under (if any), and the path.
    unnamed: Vec<(String, Option<String>, String)>,
    variants: &'a Variants,
    /// The names each module of the program declares, for a kind's methods,
    /// which its file may name with no name of the module (`include m {K}`).
    tables: &'a [Names],
    type_places: &'a TypePlaces,
    /// The program's type aliases, put in where their names are written; none
    /// while the types of the aliases themselves are read (see `State::resolve`).
    aliases: &'a TypeAliases,
    /// The names of the types the declaration being walked takes: they name no
    /// shape or kind, so a written type naming one is left as it is.
    params: Vec<String>,
    /// How this file writes the program's names, for errors naming a type.
    spelling: Spelling,
    /// Inside a pattern, the names that stand for the type following it:
    /// `this` and the pattern's own (see `THIS_TYPE`).
    this_names: Vec<String>,
}

impl Resolver<'_> {
    /// A type alias, with its name and the names in its type written as the
    /// program names them. Each name in the type names a shape, a kind, or
    /// another alias, since only here is the type of an alias nothing uses
    /// read; the checker checks the rest of it in each place it is put.
    fn type_alias(&self, alias: &mut AliasDef) -> Result<()> {
        let name = alias.name.as_str();
        if Type::builtin(name).is_some() {
            return err(alias.span, format!("'{}' is a builtin type and cannot be redefined", name));
        }
        if BUILTIN_TASKS.contains(&name) {
            return err(alias.span, format!("'{}' is a builtin task and cannot be redefined", name));
        }
        if BUILTIN_KIND_NAMES.contains(&name) {
            let what = builtin_kind_of(name).map_or_else(|| "a builtin kind".to_string(), |kind| format!("a variant of the builtin kind '{}'", kind));
            return err(alias.span, format!("'{}' is {} and cannot be redefined", name, what));
        }
        // The checker never sees an alias, so it reports no other declaration
        // of its name: the later of the two is told here.
        if let Some(entry) = self.own.get(name).filter(|entry| entry.span != alias.span) {
            let (at, first) = if (entry.span.line, entry.span.col) < (alias.span.line, alias.span.col) { (alias.span, entry.span) } else { (entry.span, alias.span) };
            return err(at, format!("'{}' is already defined at {}:{}", name, first.line, first.col));
        }
        let written = std::cell::RefCell::new(Vec::new());
        alias.ty.map_names(&|name| {
            written.borrow_mut().push(name.to_string());
            name.to_string()
        });
        for name in written.into_inner() {
            let at = self.place(alias.ty_span, &name);
            match self.lookup(&name, at, false)? {
                Some(entry) if matches!(entry.what, What::Shape | What::Kind | What::Alias) => {}
                Some(entry) if entry.what == What::Variant => {
                    let kind = entry.kind.as_deref().map_or("", unqualified);
                    return err(at, format!("'{}' is a variant, not a type; its type is its kind, '{}'", unqualified(&name), kind));
                }
                Some(entry) => return err(at, format!("'{}' is a {}, not a type", name, entry.what.word())),
                None => return err(at, format!("unknown type '{}'", name)),
            }
        }
        alias.name = self.own_full(&alias.name);
        alias.ty = self.names_in(&alias.ty, alias.ty_span)?;
        Ok(())
    }

    /// The kinds kind `k` imitates, each written as the program names it, with
    /// the aliases in it put in. Each must name a kind, with no region, since
    /// the variants it gives name none.
    /// The patterns written among them are the patterns it follows, which go
    /// to `follows`.
    fn imitated(&mut self, k: &mut KindDef) -> Result<()> {
        if k.imitates.is_empty() {
            return Ok(());
        }
        let full = self.own_full(&k.name);
        self.enter_type_params(&k.types, "kind", &full)?;
        let read = self.imitated_in(k);
        self.params.clear();
        read
    }

    fn imitated_in(&self, k: &mut KindDef) -> Result<()> {
        for (ty, at) in std::mem::take(&mut k.imitates) {
            match self.followed_pattern(&ty, at, &k.follows)? {
                Some(pattern) => k.follows.push((pattern, at)),
                None => {
                    let kind = self.imitated_kind(&ty, at, &k.name)?;
                    k.imitates.push((kind, at));
                }
            }
        }
        Ok(())
    }

    /// The patterns shape `s` follows, each written as the program names it.
    fn followed(&self, s: &mut ShapeDef) -> Result<()> {
        for (ty, at) in std::mem::take(&mut s.follows) {
            let Some(pattern) = self.followed_pattern(&ty, at, &s.follows)? else {
                let name = match &ty {
                    Type::Named(name, _) => name.clone(),
                    Type::Applied(applied) => applied.name.clone(),
                    other => return err(at, format!("'{}' is no pattern, and a shape follows only patterns, as in 'shape {}::Shaped'", other, s.name)),
                };
                let message = match self.lookup(&name, at, false)?.map(|entry| entry.what) {
                    Some(What::Kind) => format!("'{}' is a kind, and a shape follows only patterns; a kind imitates another kind, as in 'kind Mine::{}'", name, name),
                    Some(What::Shape) => format!("'{}' is a shape, and a shape follows only patterns: no shape takes another's fields or methods", name),
                    Some(other) => format!("'{}' is a {}, not a pattern; a shape follows patterns, as in 'shape {}::Shaped'", name, other.word(), s.name),
                    None if self.params.contains(&name) || s.types.iter().any(|param| param.name == name) => format!("'{}' is a type '{}' takes, not a pattern", name, s.name),
                    None => format!("unknown pattern '{}'", name),
                };
                return err(at, message);
            };
            s.follows.push((pattern, at));
        }
        Ok(())
    }

    /// The pattern `written`, at `at`, names, as the program names it, when it
    /// names one; `before` holds the patterns already followed, which none may
    /// be twice.
    fn followed_pattern(&self, written: &Type, at: Span, before: &[(Type, Span)]) -> Result<Option<Type>> {
        let (name, region, types) = match written {
            Type::Named(name, region) => (name, region, false),
            Type::Applied(applied) => (&applied.name, &applied.region, true),
            _ => return Ok(None),
        };
        let Some(entry) = self.lookup(name, at, false)?.filter(|entry| entry.what == What::Pattern) else { return Ok(None) };
        if types {
            return err(at, format!("pattern '{}' takes no types; leave out '<…>'", name));
        }
        if let Some(region) = region {
            return err(at, format!("a pattern says what a type has, and names no region; leave out '@{}'", region));
        }
        let pattern = Type::Named(entry.full, None);
        if let Some((_, first)) = before.iter().find(|(followed, _)| *followed == pattern) {
            return err(at, format!("'{}' is followed already, at {}:{}; a shape or kind follows each pattern once", name, first.line, first.col));
        }
        Ok(Some(pattern))
    }

    /// The patterns each type a declaration takes must follow, written as the
    /// program names them (see `TypeParam::patterns`).
    fn type_patterns(&self, types: &mut [TypeParam]) -> Result<()> {
        for (name, at) in types.iter_mut().flat_map(|param| param.patterns.iter_mut()) {
            match self.lookup(name, *at, false)? {
                Some(entry) if entry.what == What::Pattern => *name = entry.full,
                Some(entry) => {
                    return err(*at, format!("'{}' is a {}, not something a type can be asked to allow; the words are {}, or the name of a pattern", name, entry.what.word(), needs_list()));
                }
                None => return err(*at, format!("'{}' isn't something a type can be asked to allow; the words are {}, or the name of a pattern", name, needs_list())),
            }
        }
        Ok(())
    }

    /// A pattern, with its name and the names in its fields' types and its
    /// methods written as the program names them, and `this` and its own name
    /// the type following it (`THIS_TYPE`).
    fn pattern_def(&mut self, pattern: &mut PatternDef) -> Result<()> {
        let name = pattern.name.as_str();
        if Type::builtin(name).is_some() {
            return err(pattern.span, format!("'{}' is a builtin type and cannot be redefined", name));
        }
        if BUILTIN_TASKS.contains(&name) {
            return err(pattern.span, format!("'{}' is a builtin task and cannot be redefined", name));
        }
        if let Some(clash) = builtin_kind_clash(name) {
            return err(pattern.span, clash);
        }
        // The checker never sees a pattern among the items, so it reports no
        // other declaration of its name: the later of the two is told here.
        if let Some(entry) = self.own.get(name).filter(|entry| entry.span != pattern.span) {
            let (at, first) = if (entry.span.line, entry.span.col) < (pattern.span.line, pattern.span.col) { (pattern.span, entry.span) } else { (entry.span, pattern.span) };
            return err(at, format!("'{}' is already defined at {}:{}", name, first.line, first.col));
        }
        self.this_names = vec![THIS_WORD.to_string(), pattern.name.clone()];
        let read = self.pattern_members(pattern);
        self.this_names.clear();
        read?;
        pattern.name = self.own_full(&pattern.name);
        Ok(())
    }

    fn pattern_members(&self, pattern: &mut PatternDef) -> Result<()> {
        for field in &mut pattern.fields {
            field.ty = self.ty(&field.ty, field.ty_span)?;
        }
        for method in &mut pattern.methods {
            for param in &mut method.params {
                param.ty = self.ty(&param.ty, param.ty_span)?;
            }
            let spans: Vec<Span> = (0..method.results.len().max(1)).map(|i| method.result_span(i)).collect();
            method.ret = match &method.ret {
                Type::Results(types) => {
                    let mut resolved = Vec::new();
                    for (i, ty) in types.iter().enumerate() {
                        resolved.push(self.ty(ty, spans.get(i).copied().unwrap_or(method.span))?);
                    }
                    Type::Results(resolved)
                }
                other => self.ty(other, spans.first().copied().unwrap_or(method.span))?,
            };
            self.block(&mut method.body)?;
        }
        Ok(())
    }

    /// `written`, at `at`, a kind kind `kind` imitates, as the program names it.
    fn imitated_kind(&self, written: &Type, at: Span, kind: &str) -> Result<Type> {
        let example = format!("as in 'kind {}::Other'", kind);
        let (name, region) = match written {
            Type::Named(name, region) => (name, region),
            Type::Applied(applied) => (&applied.name, &applied.region),
            _ => return err(at, format!("'{}' is no kind, and a kind imitates only a kind, {}", written, example)),
        };
        if self.params.contains(name) {
            return err(at, format!("'{}' is a type '{}' takes, not a kind; a kind imitates a kind it names, {}", name, kind, example));
        }
        if let Some(region) = region {
            return err(at, format!("a kind has the variants of the kinds it imitates, which name no region; leave out '@{}'", region));
        }
        let what = self.lookup(name, at, false)?.map(|entry| entry.what);
        match what {
            Some(What::Kind | What::Alias) => {}
            Some(What::Shape) => return err(at, format!("'{}' is a shape, and a kind imitates only a kind, whose variants it takes", name)),
            Some(other) => return err(at, format!("'{}' is a {}, not a kind; a kind imitates a kind, {}", name, other.word(), example)),
            None => return err(at, format!("unknown kind '{}'", name)),
        }
        let read = self.ty(written, at)?;
        let named = match &read {
            Type::Named(full, None) => Some(full),
            Type::Applied(applied) if applied.region.is_none() => Some(&applied.name),
            _ => None,
        };
        // A type alias standing for one names it too.
        if !named.is_some_and(|full| self.is_kind(full)) {
            return err(at, format!("'{}' is a type alias for {}, which is no kind; a kind imitates only a kind", name, self.spelling.ty(&read)));
        }
        Ok(read)
    }

    /// Whether `full`, a name in the program, is a kind's: a builtin one, or
    /// one a module declares.
    fn is_kind(&self, full: &str) -> bool {
        builtin_kinds().iter().any(|kind| kind.name == full) || std::iter::once(&self.own).chain(self.tables).flat_map(|names| names.values()).any(|entry| entry.full == full && entry.what == What::Kind)
    }

    fn item(&mut self, item: &mut Item) -> Result<()> {
        match item {
            Item::Task(t) => {
                t.name = self.own_full(&t.name);
                self.type_patterns(&mut t.types)?;
                self.enter_type_params(&t.types, "task", &t.name)?;
                for param in &mut t.params {
                    param.ty = self.ty(&param.ty, param.ty_span)?;
                    // The value a parameter falls back on names the program's
                    // things as a field's does.
                    if let Some(value) = &mut param.default {
                        self.expr(value)?;
                    }
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
                // A decorator's call names the program's things as the body does.
                let result = t.decorators.iter_mut().chain(t.wrappers.iter_mut()).try_for_each(|call| self.decorator(call)).and_then(|()| self.block(&mut t.body));
                self.params.clear();
                result
            }
            Item::Shape(s) => {
                s.name = self.own_full(&s.name);
                self.type_patterns(&mut s.types)?;
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
                self.type_patterns(&mut k.types)?;
                self.enter_type_params(&k.types, "kind", &k.name)?;
                for variant in &mut k.variants {
                    for field in &mut variant.payload {
                        field.ty = self.ty(&field.ty, field.ty_span)?;
                    }
                }
                self.params.clear();
                Ok(())
            }
            Item::Macro(m) => {
                m.name = self.own_full(&m.name);
                self.expr(&mut m.value)
            }
        }
    }

    /// Brings the types a declaration takes into scope, so the types written in
    /// it name them rather than a shape or kind of the same name, which would be
    /// two types under one name.
    fn enter_type_params(&mut self, types: &[TypeParam], what: &str, name: &str) -> Result<()> {
        for param in types {
            if let Some(entry) = self.lookup(&param.name, param.span, false)?
                && matches!(entry.what, What::Shape | What::Kind | What::Alias)
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
                    found => {
                        let hint = match self.kind_hint(first) {
                            hint if hint.is_empty() => self.unnamed_hint(written, first, calls),
                            hint => hint,
                        };
                        let nor = if found.is_none() { "a shape or a kind this file sees" } else { "a kind" };
                        err(at, format!("'{}' names nothing: '{}' is neither a module this file includes nor {}{}", written, first, nor, hint))
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
            return err(at, format!("module '{}' has no task, shape, kind, type alias, variant, or macro named '{}'", module_name, member));
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
            None => {
                // `Kind.m` without `(…)`, naming a method of the kind.
                let method = method_name(&kind.full, tag);
                match self.has_method(&method) {
                    true => err(at, format!("'{0}.{1}' is a method of kind '{0}', so it is called, as in '{0}.{1}(…)' when it is shared, or 'k.{1}(…)' on a value", kind_name, tag)),
                    false => err(at, format!("kind '{}' has no variant '{}'", kind_name, tag)),
                }
            }
        }
    }

    /// Whether the program declares method `method` (its name in the program).
    fn has_method(&self, method: &str) -> bool {
        std::iter::once(&self.own).chain(self.tables).flat_map(|names| names.values()).any(|entry| entry.full == method)
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

    /// Each part of a condition joined with `and`.
    fn condition(&self, cond: &mut Condition) -> Result<()> {
        let mut part = Some(cond);
        while let Some(current) = part {
            self.expr(&mut current.value)?;
            current.pattern.iter_mut().try_for_each(|pattern| self.pattern(pattern))?;
            part = current.and.as_deref_mut();
        }
        Ok(())
    }

    /// A pattern's kind, as in `case Shape.Circle`, rewritten to its name in the
    /// program.
    fn pattern(&self, pattern: &mut Pattern) -> Result<()> {
        // `maybe geometry.Point {p}`: the type a test names, as anywhere.
        if let PatternKind::Typed { ty, .. } = &mut pattern.kind {
            *ty = self.ty(ty, pattern.span)?;
            return Ok(());
        }
        if let PatternKind::Variant { kind: Some(kind), .. } = &mut pattern.kind {
            let (name, at) = &mut **kind;
            match self.kind(name, *at)? {
                Some(kind) => *name = kind.full,
                None => match name.split_once(MODULE_SEPARATOR) {
                    Some((first, _)) if !self.modules.contains_key(first) => {
                        return err(*at, format!("'{}' names nothing: '{}' isn't a module this file includes{}", name, first, self.unnamed_hint(name, first, false)));
                    }
                    _ => return err(*at, format!("'{}' isn't a kind, so a case form can't name a variant of it{}", name, self.kind_hint(name))),
                },
            }
        }
        Ok(())
    }

    /// A decorator's call, `d(…)`, whose name may be a shared method's,
    /// `X.m` or `m.X.m`, as a call of one is written.
    fn decorator(&self, call: &mut Expr) -> Result<()> {
        // `$(.checked)`: a method of the value, which the checker finds on the
        // wrapped method's type.
        if let ExprKind::Call { name, args, .. } = &mut call.kind
            && name.starts_with(MEMBER_WRAPPER)
        {
            return args.iter_mut().try_for_each(|arg| self.expr(arg));
        }
        if let ExprKind::Call { name, args, .. } = &mut call.kind
            && name.contains(MODULE_SEPARATOR)
            && let Some(method) = self.shared_method(&name.split(MODULE_SEPARATOR).collect::<Vec<_>>(), call.span)?
        {
            *name = method;
            return args.iter_mut().try_for_each(|arg| self.expr(arg));
        }
        // `Kind.Tag`, a variant, makes a value; a decorator is a task.
        if let ExprKind::Call { name, .. } = &call.kind
            && name.contains(MODULE_SEPARATOR)
            && self.lookup(name, call.span, true)?.is_some_and(|entry| entry.what == What::Variant)
        {
            return err(call.span, format!("'{}' is a variant, which makes a value, so it can't be a decorator; a decorator is a task or a shared method", name));
        }
        self.expr(call)
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
                !self.modules.get(*module).and_then(|names| names.names.get(*second)).is_some_and(|entry| matches!(entry.what, What::Variant | What::Macro))
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
        // `m.P`: a macro of a module this file includes.
        if let Some(module) = self.modules.get(name.as_str()) {
            let entry = self.member(name, module, tag, expr.span)?;
            // `m.square`: a task of the module, named as a value.
            if matches!(entry.what, What::Macro | What::Task) {
                expr.kind = ExprKind::Var(entry.full);
                return Ok(true);
            }
        }
        if let Some(kind) = self.kind(name, base.span)? {
            // `Kind.m`, a shared method named as a value, which the checker
            // knows from a method of every value (see `Checker::check_shared_use`).
            let method = method_name(&kind.full, tag);
            if !self.variants.get(&kind.full).is_some_and(|tags| tags.contains_key(tag.as_str())) && self.has_method(&method) {
                expr.kind = ExprKind::Shared { shape: kind.full, field: tag.clone() };
                return Ok(true);
            }
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
            // `P.len`: a field of a macro's value.
            if entry.what == What::Macro {
                return Ok(false);
            }
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

    /// A written type, at `span`, with its shape and kind names rewritten and
    /// each type alias put in (see `expand_aliases`).
    fn ty(&self, ty: &Type, span: Span) -> Result<Type> {
        let ty = self.names_in(ty, span)?;
        if self.aliases.is_empty() {
            return Ok(ty);
        }
        let at = |alias: &str| self.alias_place(span, alias);
        let mut unplaced = Vec::new();
        let expanded = expand_aliases(&ty, self.aliases, &mut Vec::new(), &mut unplaced, &at)?;
        // The parser gives a region written after a tuple, an array, a slice,
        // a map, a pointer or a shape to each name in it too, not knowing which
        // is an alias's (`(Cents, text)@r` is `(Cents@r, text@r)`), so an alias
        // of a type that lives in no region may have one only where the rest of
        // the type lives in it.
        let regions = expanded.regions();
        if let Some((region, alias, aliased)) = unplaced.iter().find(|(region, _, _)| !regions.contains(&region.as_str())) {
            return err(at(alias), format!("'{}' holds no pointers or text, so it lives in no region; remove '@{}'", aliased, region));
        }
        Ok(expanded)
    }

    /// Where the part `name` of the type written at `span` is (see
    /// `TypePlaces`), or `span` when it has no place of its own.
    fn place(&self, span: Span, name: &str) -> Span {
        self.type_places.get(&span.place_key()).and_then(|parts| parts.iter().find(|(part, _)| part == name)).map_or(span, |(_, at)| *at)
    }

    /// Where the type written at `span` names the type alias `alias` (its name
    /// in the program), or `span` when it is named in another alias.
    fn alias_place(&self, span: Span, alias: &str) -> Span {
        let parts = self.type_places.get(&span.place_key()).map_or(&[][..], Vec::as_slice);
        parts.iter().find(|(part, at)| self.lookup(part, *at, false).ok().flatten().is_some_and(|entry| entry.full == alias)).map_or(span, |(_, at)| *at)
    }

    /// A written type, at `span`, with its shape, kind, and type alias names
    /// rewritten.
    fn names_in(&self, ty: &Type, span: Span) -> Result<Type> {
        let task = |task: &TaskType| -> Result<TaskType> {
            let params = task.params.iter().map(|t| self.names_in(t, span)).collect::<Result<_>>()?;
            Ok(TaskType { params, taken: task.taken.clone(), ret: self.names_in(&task.ret, span)? })
        };
        Ok(match ty {
            // In a pattern, the type following it.
            Type::Named(name, region) if self.this_names.contains(name) => Type::Named(THIS_TYPE.to_string(), region.clone()),
            Type::Applied(applied) if self.this_names.contains(&applied.name) => {
                return err(self.place(span, &applied.name), format!("'{}' is the type following this pattern, which takes no types; leave out '<…>'", applied.name));
            }
            // A type the declaration takes, not a shape or kind (see
            // `enter_type_params`).
            Type::Named(name, _) if self.params.contains(name) => ty.clone(),
            Type::Applied(applied) => {
                let at = self.place(span, &applied.name);
                if self.lookup(&applied.name, at, false)?.is_some_and(|entry| entry.what == What::Alias) {
                    return err(at, format!("type alias '{}' takes no types: it stands for one type, so leave out '<…>'", applied.name));
                }
                let mut name = applied.name.clone();
                self.rename(&mut name, at, &[What::Shape, What::Kind])?;
                let types = applied.types.iter().map(|ty| self.names_in(ty, span)).collect::<Result<_>>()?;
                Type::Applied(Box::new(Applied { name, types, region: applied.region.clone() }))
            }
            Type::Named(name, region) => {
                let at = self.place(span, name);
                let mut name = name.clone();
                self.rename(&mut name, at, &[What::Shape, What::Kind, What::Alias])?;
                Type::Named(name, region.clone())
            }
            Type::Ptr(inner, region) => Type::Ptr(Box::new(self.names_in(inner, span)?), region.clone()),
            Type::Maybe(inner) => Type::Maybe(Box::new(self.names_in(inner, span)?)),
            Type::Array(inner, count) => Type::Array(Box::new(self.names_in(inner, span)?), *count),
            Type::Slice(inner, region) => Type::Slice(Box::new(self.names_in(inner, span)?), region.clone()),
            Type::Range(inner) => Type::Range(Box::new(self.names_in(inner, span)?)),
            Type::Fail(inner) => Type::Fail(Box::new(self.names_in(inner, span)?)),
            Type::Or(value, error) => Type::Or(Box::new(self.names_in(value, span)?), Box::new(self.names_in(error, span)?)),
            Type::Results(types) => Type::Results(types.iter().map(|t| self.names_in(t, span)).collect::<Result<_>>()?),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| self.names_in(t, span)).collect::<Result<_>>()?),
            Type::Map(key, value, region) => Type::Map(Box::new(self.names_in(key, span)?), Box::new(self.names_in(value, span)?), region.clone()),
            Type::Task(t) => Type::Task(Box::new(task(t)?)),
            Type::Closure(closure) => Type::Closure(Box::new(ClosureType { task: task(&closure.task)?, region: closure.region.clone(), mode: closure.mode })),
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
            StmtKind::Step { target, value, .. } => {
                self.expr(target)?;
                self.expr(value)
            }
            StmtKind::Binding { names, init, .. } => {
                names.iter_mut().try_for_each(|bound| self.bound(bound))?;
                init.iter_mut().try_for_each(|e| self.expr(e))
            }
            StmtKind::Assign(targets, values) => {
                targets.iter_mut().try_for_each(|target| self.target(target))?;
                values.iter_mut().try_for_each(|e| self.expr(e))
            }
            StmtKind::When(arms, else_block) => {
                for arm in arms.iter_mut() {
                    self.condition(&mut arm.cond)?;
                    self.block(&mut arm.body)?;
                }
                match else_block {
                    Some(block) => self.block(block),
                    None => Ok(()),
                }
            }
            StmtKind::Block(body) => self.block(body),
            StmtKind::Turns(turns) => turns.iter_mut().try_for_each(|turn| self.block(turn)),
            StmtKind::Named(_, inner) => self.stmt(inner),
            StmtKind::Loop(cond, body) => {
                self.condition(cond)?;
                self.block(body)
            }
            StmtKind::Each(_, _, range, body) => {
                self.expr(range)?;
                self.block(body)
            }
            StmtKind::Parted(parted) => {
                parted.start.iter_mut().try_for_each(|stmt| self.stmt(stmt))?;
                if let Some(cond) = &mut parted.cond {
                    self.condition(cond)?;
                }
                parted.step.iter_mut().try_for_each(|stmt| self.stmt(stmt))?;
                self.block(&mut parted.body)
            }
            StmtKind::Match(value, cases) => {
                self.expr(value)?;
                cases.iter_mut().try_for_each(|case| self.pattern(&mut case.pattern))?;
                cases.iter_mut().try_for_each(|case| self.block(&mut case.body))
            }
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            StmtKind::Break(_, Some(value)) => self.expr(value),
            StmtKind::Region { .. } | StmtKind::Break(..) | StmtKind::Continue(_) => Ok(()),
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
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::RegionAfter(_) => Ok(()),
            // Only the resolver makes one, above.
            ExprKind::Shared { .. } | ExprKind::TypeOf(_) | ExprKind::Macro { .. } | ExprKind::TaskValue(_) => Ok(()),
            ExprKind::Spread(inner) => self.expr(inner),
            // The lambda is a task of the file, named as its variable would be.
            ExprKind::Closure { lambda, .. } => self.rename(lambda, span, &[What::Variant, What::Macro, What::Task]),
            ExprKind::Kept { value, .. } => self.expr(value),
            ExprKind::Invoke { callee, args } => {
                self.expr(callee)?;
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            // A variable keeps its name. No variable can be named like a variant
            // or a macro (see `Checker::define`), so one written as a value is renamed.
            ExprKind::Var(name) => self.rename(name, span, &[What::Variant, What::Macro]),
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
                // An alias names a type, and a value is written with the
                // shape's own name.
                if let Some(entry) = self.lookup(name, span, false)?.filter(|entry| entry.what == What::Alias)
                    && self.aliases.contains_key(&entry.full)
                {
                    let aliased = expand_aliases(&Type::Named(entry.full, None), self.aliases, &mut Vec::new(), &mut Vec::new(), &|_| span)?;
                    let message = match self.spelling.ty(&aliased) {
                        Type::Named(shape, _) => format!("'{0}' is a type alias, a name for '{1}'; a value is written with the shape's own name, as in '{1} {{ … }}'", name, shape),
                        aliased => format!("'{}' is a type alias, a name for the type '{}', so it makes no value", name, aliased),
                    };
                    return err(span, message);
                }
                self.rename(name, span, &[What::Shape, What::Kind])?;
                // The types a literal names are written types too.
                for (ty, at) in types.iter_mut() {
                    *ty = self.ty(&ty.clone(), *at)?;
                }
                fields.iter_mut().try_for_each(|field| self.expr(&mut field.value))
            }
            // `Option<int>.None`: the kind and its variant, named as the file
            // writes them, and the types written types too.
            ExprKind::Construct { kind, tag, args, types } if !types.is_empty() => {
                let Some(entry) = self.kind(kind, span)? else {
                    let what = match self.lookup(kind, span, false)? {
                        Some(entry) if entry.what == What::Shape => format!("'{0}' is a shape, so its values are written '{0}<…> {{ … }}'", kind),
                        Some(entry) => format!("'{}' is a {}, not a kind, so it has no variant '{}'", kind, entry.what.word(), tag),
                        // `(a < b, c >.x)`: two comparisons, read as a kind given types.
                        None if types.len() > 1 => format!(
                            "unknown kind '{0}'; if '{0} < …, … >.{1}' compares, put the comparison before the ',' in parentheses, as in '({0} < …), … >.{1}'",
                            kind, tag
                        ),
                        None => format!("unknown kind '{}'", kind),
                    };
                    return err(span, what);
                };
                self.variant(&entry, kind, tag, span)?;
                *kind = entry.full;
                for (ty, at) in types.iter_mut() {
                    *ty = self.ty(&ty.clone(), *at)?;
                }
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            ExprKind::Construct { args, .. } => args.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Field(base, _) | ExprKind::Deref(base) | ExprKind::Ref(base) | ExprKind::Unary(_, base) | ExprKind::Fail(base) | ExprKind::Wrap(base) | ExprKind::Widen(base) | ExprKind::Pipe(base) | ExprKind::Piped(base) => self.expr(base),
            ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Array(values) | ExprKind::Tuple(values) => values.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Pairs(pairs) => pairs.iter_mut().try_for_each(|(key, value)| {
                self.expr(key)?;
                self.expr(value)
            }),
            ExprKind::MapOp { map, key, .. } => {
                self.expr(map)?;
                key.as_deref_mut().map_or(Ok(()), |key| self.expr(key))
            }
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
            ExprKind::LoopValue(value) => self.stmt(&mut value.stmt),
            ExprKind::Edit { items, args, .. } => {
                self.expr(items)?;
                args.iter_mut().try_for_each(|e| self.expr(e))
            }
            ExprKind::Scan { args, .. } | ExprKind::Join { parts: args, .. } => args.iter_mut().try_for_each(|e| self.expr(e)),
            ExprKind::Copy { value, .. } | ExprKind::Shortcut { value, .. } | ExprKind::ToText { value, .. } => self.expr(value),
        }
    }

    /// `Kind.add(…)`, `Kind.del(…)`, `Kind.added(…)`, `Kind.without(…)`, or
    /// `Kind.copy()`, which the parser reads as
    /// a change to a slice or a copy, rewritten to the call of the kind's
    /// variant; whether `expr` was one. No variable can be named like a kind
    /// (see `Checker::define`). (A function of its own: `expr`'s frame is on the
    /// stack once per nesting level.)
    fn edit_as_variant(&self, expr: &mut Expr) -> Result<bool> {
        let (items, word) = match &expr.kind {
            ExprKind::Edit { items, op, .. } => (items, op.word()),
            // `Kind.added(…)` and `Kind.without(…)`, which the parser reads as
            // a copy made with a change.
            ExprKind::Copy { value, region: None, .. } => match &value.kind {
                ExprKind::Edit { items, op, .. } if op.makes_copy() => (items, op.word()),
                _ => (value, COPY),
            },
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
            ExprKind::Copy { value, .. } => match &mut value.kind {
                ExprKind::Edit { args, .. } => std::mem::take(args),
                _ => Vec::new(),
            },
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
