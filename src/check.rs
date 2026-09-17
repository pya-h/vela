use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Error, Result};
use crate::modules::{IncludeScope, ModuleScope, Program};

#[derive(Clone)]
struct VariantInfo {
    /// The kind the variant belongs to.
    kind: String,
    variant: Variant,
}

#[derive(Clone)]
struct VarInfo {
    /// `Type::Region` for a region.
    ty: Type,
    mutable: bool,
    /// Where the variable was declared.
    span: Span,
    /// For a region: whether `new` can allocate in it. A region a task only
    /// names in its header's types (`p: ^int@r`) can't be.
    allocates: bool,
    /// Whether this is a named result, which only `<-` sets.
    result: bool,
}

/// Where a type is written, which decides the regions it may name.
#[derive(Clone, Copy, PartialEq)]
enum TypePlace {
    /// A shape's field or a variant's payload: no regions, since fields live in
    /// the region of the value holding them.
    Field,
    /// The type after `new`, which lives in the region `new` allocates in.
    New,
    /// A task's header: any region names, which the task is given.
    Header,
    /// Inside a task: only regions visible there.
    Body,
}

/// Type-checks a program and annotates it for code generation: every
/// expression gets its `ty`, and every binding without an annotation gets its
/// inferred type. Code generation relies on these annotations.
#[derive(Default)]
pub struct Checker {
    shapes: HashMap<String, ShapeDef>,
    kinds: HashMap<String, KindDef>,
    /// Every task's parameters (names and types) and return type.
    tasks: HashMap<String, (Vec<(String, Type)>, Type)>,
    /// The shapes and kinds that hold pointers, directly or in other shapes and
    /// kinds, so their values live in a region.
    regional: HashSet<String>,
    /// Every variant, by its name in the program: its tag, qualified with its
    /// kind's module outside the main file (`geometry.Circle`). Tags are unique
    /// across the kinds of a module.
    variants: HashMap<String, VariantInfo>,
    /// Each module of the program, by name; the main file's is empty.
    modules: HashMap<String, ModuleScope>,
    /// The tasks, shapes, kinds, and variants marked `priv` (a variant through
    /// its kind), by their names in the program.
    private: HashSet<String>,
    /// The module whose code is being checked.
    current_module: String,
    scopes: Vec<HashMap<String, VarInfo>>,
    current_ret: Option<Type>,
    /// The names of the current task's results, in order, when it names them.
    result_names: Vec<String>,
    /// Those of them not set on the path being checked. Vela has no zero
    /// values, so a result must be set before it is read, before a bare `<-`,
    /// and before the end of the task, on every path.
    unset: HashSet<String>,
    /// Where the parts of written types are, from the parser.
    type_places: TypePlaces,
    /// At most how many bytes a value of each shape and kind takes, once the
    /// declarations are checked (see `check_sizes`).
    sizes: HashMap<String, u128>,
}

/// The most memory a value may take, in bytes: 1 TiB. A larger type is an
/// error, so that no C compiler rejects the program for it.
const MAX_VALUE_BYTES: u128 = 1 << 40;

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A checker that points errors about a part of a written type (a region,
    /// a type name, the error type) at that part, using the parser's
    /// `Parser::type_places`.
    pub fn with_type_places(type_places: TypePlaces) -> Self {
        Checker { type_places, ..Self::default() }
    }

    /// Checks a program loaded with `modules::load`, and annotates its items.
    pub fn check_program(program: &mut Program) -> Result<()> {
        let mut checker = Checker::with_type_places(std::mem::take(&mut program.type_places));
        checker.modules = program.modules.iter().map(|scope| (scope.name.clone(), scope.clone())).collect();
        let result = checker.check_file(&mut program.items);
        program.type_places = checker.type_places;
        // Names in a message are written as the file with the error writes them.
        result.map_err(|mut error| {
            if let Some(scope) = error.span.and_then(|span| program.modules.iter().find(|scope| scope.file == span.file)) {
                error.message = scope.spelling.text(&error.message);
            }
            error
        })
    }

    /// Whether a task, shape, kind, or variant name written in the current
    /// module can name the declaration called `name` in the program. The
    /// resolver has given every name a module's code can use its full name, so a
    /// name without a module is one of the main file's only in the main file.
    fn sees(&self, name: &str) -> bool {
        self.current_module.is_empty() || name.contains(MODULE_SEPARATOR)
    }

    /// The program's name for `name` written unqualified in the current module:
    /// what `include` brings in under it, or else a name of the module itself.
    fn in_module(&self, name: &str) -> String {
        if let Some(full) = self.modules.get(&self.current_module).and_then(|scope| scope.imported(name)) {
            return full.to_string();
        }
        qualify(&self.current_module, name)
    }

    /// The program's name for a task, shape, kind, or variant name written in
    /// the current module: itself when qualified, and `in_module` otherwise.
    fn full_name(&self, name: &str) -> String {
        if name.contains(MODULE_SEPARATOR) { name.to_string() } else { self.in_module(name) }
    }

    /// The variant `name`, written unqualified in the current module, stands for.
    fn variant_in_scope(&self, name: &str) -> Option<&VariantInfo> {
        self.variants.get(&self.in_module(name))
    }

    /// A hint for a name written in the current module that names nothing
    /// here, but that a module it includes declares: `; module 'm' has one: …`.
    /// Names of the program in it are written as the program names them, and
    /// the message is spelled for the file later (see `check_program`).
    fn elsewhere(&self, name: &str) -> String {
        let Some(scope) = self.modules.get(&self.current_module) else { return String::new() };
        let declared = |full: &str| {
            !self.private.contains(full) && (self.tasks.contains_key(full) || self.shapes.contains_key(full) || self.kinds.contains_key(full) || self.variants.contains_key(full))
        };
        let Some(include) = scope.includes.iter().find(|include| declared(&qualify(&include.id, name))) else { return String::new() };
        let full = qualify(&include.id, name);
        if let Some((alias, _, at)) = scope.imports.iter().find(|(written, imported, _)| *imported == full && written != name) {
            return format!("; it is brought in as '{}' at {}:{}", alias, at.line, at.col);
        }
        match &include.local {
            Some(module) => format!("; module '{}' has one: write '{}', or bring it in with 'include … ({})'", module, full, name),
            None => format!("; module '{}' has one: list it in 'include {} {{…}}'", include.written, include.written),
        }
    }

    /// A hint for a variable name that names nothing, but that a module the
    /// current module includes would be named by, without its alias or braces.
    fn module_elsewhere(&self, name: &str) -> String {
        let Some(scope) = self.modules.get(&self.current_module) else { return String::new() };
        match scope.includes.iter().find(|include| include.file_name == name && include.local.as_deref() != Some(name)) {
            Some(IncludeScope { local: Some(alias), written, .. }) => format!("; module '{}' is included as '{}'", written, alias),
            Some(include) => format!("; module '{}' is included with only its parts, which are named alone", include.written),
            None => String::new(),
        }
    }

    /// Where the current module includes the module it names `name`, if it does.
    fn module_used(&self, name: &str) -> Option<Span> {
        let scope = self.modules.get(&self.current_module)?;
        scope.included(name).map(|include| include.span)
    }

    /// Where `part` (see `TypePlaces`) is in the type written at `span`, or
    /// `span` itself if it isn't known.
    fn place(&self, span: Span, part: &str) -> Span {
        let parts = self.type_places.get(&span.place_key());
        // A shape or kind of another module may be written without its module,
        // as in its own file or after `include m (Name)`.
        let found = |part: &str| parts.and_then(|parts| parts.iter().find(|(name, _)| name == part)).map(|(_, at)| *at);
        let spelled = self.modules.get(&self.current_module).map(|scope| scope.spelling.name(part));
        found(part).or_else(|| spelled.and_then(|spelled| found(&spelled))).or_else(|| found(unqualified(part))).unwrap_or(span)
    }

    pub fn check_file(&mut self, items: &mut [Item]) -> Result<()> {
        self.collect_declarations(items)?;
        self.regional = regional_types(items);
        self.check_declarations(items)?;
        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.current_module = module_of(&t.name).to_string();
                self.check_task(t)?;
            }
        }
        self.current_module.clear();
        Ok(())
    }

    /// Registers every task, shape, kind, and variant. They share one
    /// namespace: no two may have the same name, and none may use the name of
    /// a builtin type or task.
    fn collect_declarations(&mut self, items: &[Item]) -> Result<()> {
        let mut names: HashMap<String, Span> = HashMap::new();
        let mut declare = |name: &str, span: Span| -> Result<()> {
            // No declaration may take the name of a builtin type or task.
            let written = unqualified(name);
            if Type::builtin(written).is_some() {
                return err(span, format!("'{}' is a builtin type and cannot be redefined", written));
            }
            if BUILTIN_TASKS.contains(&written) {
                return err(span, format!("'{}' is a builtin task and cannot be redefined", written));
            }
            if let Some(at) = names.get(name) {
                return err(span, format!("'{}' is already defined at {}:{}", written, at.line, at.col));
            }
            names.insert(name.to_string(), span);
            Ok(())
        };
        for item in items {
            let private = match item {
                Item::Task(t) => t.private,
                Item::Shape(s) => s.private,
                Item::Kind(k) => k.private,
            };
            if private {
                let name = match item {
                    Item::Task(t) => &t.name,
                    Item::Shape(s) => &s.name,
                    Item::Kind(k) => &k.name,
                };
                self.private.insert(name.clone());
                if let Item::Kind(k) = item {
                    self.private.extend(k.variants.iter().map(|v| qualify(module_of(&k.name), &v.tag)));
                }
            }
            match item {
                Item::Shape(s) => {
                    declare(&s.name, s.span)?;
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    declare(&k.name, k.span)?;
                    for v in &k.variants {
                        let full = qualify(module_of(&k.name), &v.tag);
                        declare(&full, v.span)?;
                        let info = VariantInfo { kind: k.name.clone(), variant: v.clone() };
                        self.variants.insert(full, info);
                    }
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Task(t) => {
                    declare(&t.name, t.span)?;
                    let params = t.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                    self.tasks.insert(t.name.clone(), (params, t.ret.clone()));
                }
            }
        }
        Ok(())
    }

    /// Checks the declarations themselves: every written type exists, field
    /// names are distinct, kinds have variants, and no type contains itself.
    /// A task's header gets its types with regions settled (see `resolve_type`).
    fn check_declarations(&mut self, items: &mut [Item]) -> Result<()> {
        for item in items.iter_mut() {
            let name = match item {
                Item::Task(t) => &t.name,
                Item::Shape(s) => &s.name,
                Item::Kind(k) => &k.name,
            };
            self.current_module = module_of(name).to_string();
            self.check_private_types(item)?;
            match item {
                Item::Shape(s) => self.check_fields(&s.fields)?,
                Item::Kind(k) => {
                    if k.variants.is_empty() {
                        return err(k.span, format!("kind '{}' has no variants", k.name));
                    }
                    for v in &k.variants {
                        self.check_fields(&v.payload)?;
                    }
                }
                Item::Task(t) => {
                    self.check_header(t)?;
                    if t.name == "main" {
                        if let Some(param) = t.params.first() {
                            return err(param.span, "task 'main' cannot take parameters");
                        }
                        if t.ret != Type::Unit {
                            return err(t.ret_span(), "task 'main' cannot return a value");
                        }
                    }
                }
            }
        }
        let mut acyclic = HashSet::new();
        let mut order = Vec::new();
        for item in items.iter() {
            let name = match item {
                Item::Shape(s) => &s.name,
                Item::Kind(k) => &k.name,
                Item::Task(_) => continue,
            };
            self.check_value_cycle(name, &mut acyclic, &mut order)?;
        }
        self.current_module.clear();
        self.check_sizes(&order)?;
        self.check_constructible(items)
    }

    /// Settles how large a value of each shape and kind can be, in `order`,
    /// where each comes after the types it holds by value, and rejects one too
    /// large for memory (see `MAX_VALUE_BYTES`).
    fn check_sizes(&mut self, order: &[String]) -> Result<()> {
        for name in order {
            let (bytes, span) = match (self.shapes.get(name), self.kinds.get(name)) {
                (Some(shape), _) => (fields_bytes(&shape.fields, &self.sizes), shape.span),
                (None, Some(kind)) => {
                    let largest = kind.variants.iter().map(|v| fields_bytes(&v.payload, &self.sizes)).max().unwrap_or(0);
                    (largest.saturating_add(8), kind.span)
                }
                (None, None) => continue,
            };
            if bytes > MAX_VALUE_BYTES {
                return err(span, format!("'{}' is too large: a value of it would take more than 1 TiB of memory", unqualified(name)));
            }
            self.sizes.insert(name.clone(), bytes);
        }
        Ok(())
    }

    /// Rejects a type written at `span` whose values would take more than
    /// `MAX_VALUE_BYTES`, such as `[[int; 1000000]; 1000000]`.
    fn check_size(&self, ty: &Type, span: Span) -> Result<()> {
        if value_bytes(ty, &self.sizes) > MAX_VALUE_BYTES {
            return err(span, format!("{} is too large: a value of it would take more than 1 TiB of memory; a slice in a region can be as long as memory allows", ty));
        }
        Ok(())
    }

    /// Rejects a private type in what other files see of a public item: a
    /// task's parameters and results, a shape's public fields, and a kind's
    /// payloads. Code using the item couldn't name the type.
    fn check_private_types(&self, item: &Item) -> Result<()> {
        let (private, what, name, fields): (bool, &str, &str, Vec<&Field>) = match item {
            Item::Task(t) => (t.private, "task", &t.name, t.params.iter().collect()),
            Item::Shape(s) => (s.private, "shape", &s.name, s.fields.iter().filter(|f| !f.private).collect()),
            Item::Kind(k) => (k.private, "kind", &k.name, k.variants.iter().flat_map(|v| &v.payload).collect()),
        };
        if private {
            return Ok(());
        }
        let mut written: Vec<(&Type, Span)> = fields.iter().map(|f| (&f.ty, f.ty_span)).collect();
        if let Item::Task(t) = item {
            written.extend(t.ret.results().iter().enumerate().map(|(i, ty)| (ty, t.result_span(i))));
        }
        for (ty, span) in written {
            let hidden = named_types(ty).into_iter().find(|n| self.shapes.get(*n).is_some_and(|s| s.private) || self.kinds.get(*n).is_some_and(|k| k.private));
            if let Some(hidden) = hidden {
                let fix = if matches!(item, Item::Shape(_)) { "make the field 'priv' too, or the type public" } else { "make it 'priv' too, or the type public" };
                return err(
                    self.place(span, hidden),
                    format!("{} '{}' is public, but '{}' is private to its file, so other files couldn't name it; {}", what, unqualified(name), unqualified(hidden), fix),
                );
            }
        }
        Ok(())
    }

    /// Checks a shape's fields or a variant's payload.
    fn check_fields(&self, fields: &[Field]) -> Result<()> {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for field in fields {
            if let Some(at) = seen.get(field.name.as_str()) {
                return err(field.span, format!("field '{}' is already defined at {}:{}", field.name, at.line, at.col));
            }
            seen.insert(&field.name, field.span);
            self.resolve_type(&field.ty, field.ty_span, TypePlace::Field)?;
        }
        Ok(())
    }

    /// Checks a task's parameters and result types, and settles their regions.
    /// A region the header names without a `region` parameter is one the task
    /// only reads or links pointers in (it can't allocate there), and each
    /// result's regions must be regions the task is given.
    fn check_header(&mut self, t: &mut TaskDef) -> Result<()> {
        let region_params: Vec<&str> = t.params.iter().filter(|p| p.ty == Type::Region).map(|p| p.name.as_str()).collect();
        let mut named: Vec<String> = region_params.iter().map(|name| name.to_string()).collect();
        let mut resolved = Vec::new();
        for param in &t.params {
            if param.ty == Type::Region {
                resolved.push(Type::Region);
                continue;
            }
            let ty = self.resolve_type(&param.ty, param.ty_span, TypePlace::Header)?;
            for region in ty.regions() {
                if let Some(other) = t.params.iter().find(|p| p.name == region && p.ty != Type::Region) {
                    return err(self.place(param.ty_span, &format!("@{}", region)), format!("'{}' is a parameter, not a region", other.name));
                }
                if !named.iter().any(|n| n == region) {
                    named.push(region.to_string());
                }
            }
            resolved.push(ty);
        }
        let several = matches!(t.ret, Type::Results(_));
        let written: Vec<(Type, Span)> = t.ret.results().iter().cloned().zip((0..).map(|i| t.result_span(i))).collect();
        let mut results = Vec::new();
        for (i, (result, span)) in written.into_iter().enumerate() {
            if result == Type::Region {
                return err(span, "a task can't send a region; regions are only parameters, as in 'task build(r: region)'");
            }
            if result == Type::Unit {
                return err(span, "unit is only allowed as a task's whole return type, so it can't be one of several results");
            }
            let ret = self.resolve_type(&result, span, TypePlace::Header)?;
            if let Some(region) = ret.regions().into_iter().find(|r| !named.iter().any(|n| n == r)) {
                let at = self.place(span, &format!("@{}", region));
                if t.params.iter().any(|p| p.name == region) {
                    return err(at, format!("'{}' is a parameter, not a region", region));
                }
                // A named result is easier to point at by its name than by its
                // place in the list.
                let which = match t.results.get(i).and_then(|r| r.name.as_ref()) {
                    Some((name, _)) => format!("result '{}'", name),
                    None if several => format!("result {}", i + 1),
                    None => "the result".to_string(),
                };
                return err(
                    at,
                    format!(
                        "{1} lives in region '{0}', which the task isn't given; take it as a parameter, as in '{0}: region'",
                        region, which
                    ),
                );
            }
            results.push(ret);
        }
        t.ret = match results.len() {
            0 => Type::Unit,
            1 => results.into_iter().next().unwrap_or(Type::Unit),
            _ => Type::Results(results),
        };
        // A `unit or E` task succeeds by reaching its end or a bare `<-`, so a
        // name for its result would have nothing to hold.
        if !sends_value(&t.ret)
            && let Some((name, at)) = t.result_names().next()
        {
            return err(*at, format!("a '{}' task succeeds by reaching its end or a bare '<-', so '{}' has no value to name", t.ret, name));
        }
        for (param, ty) in t.params.iter_mut().zip(resolved) {
            param.ty = ty;
        }
        let params = t.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
        self.tasks.insert(t.name.clone(), (params, t.ret.clone()));
        Ok(())
    }

    /// Checks a written type and settles its regions for `place`:
    /// - in a field or after `new`, no type names a region;
    /// - elsewhere, every pointer and every regional shape or kind names one,
    ///   which inside a task must be a region visible there;
    /// - a shape or kind without pointers lives in no region, so `Point@r` is
    ///   an error, while the `Point` in `^Point@r` just has its region dropped.
    fn resolve_type(&self, ty: &Type, span: Span, place: TypePlace) -> Result<Type> {
        self.check_type(ty, span)?;
        // The sizes of shapes and kinds are known inside tasks; fields and
        // headers are checked for size once they are (see `check_task`).
        if matches!(place, TypePlace::Body | TypePlace::New) {
            self.check_size(ty, span)?;
        }
        self.resolve_regions(ty, span, place, None)
    }

    /// `resolve_type` for `ty` inside a pointer of region `inside`.
    fn resolve_regions(&self, ty: &Type, span: Span, place: TypePlace, inside: Option<&str>) -> Result<Type> {
        let given = |region: &Option<String>| -> Result<()> {
            match (place, region) {
                (TypePlace::Field, Some(region)) => err(
                    self.place(span, &format!("@{}", region)),
                    format!("a field lives in the region of the value holding it, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::New, Some(region)) => err(
                    self.place(span, &format!("@{}", region)),
                    format!("the value 'new' makes lives in the region it allocates in, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::Body, Some(region)) => self.visible_region(region, self.place(span, &format!("@{}", region)), false),
                _ => Ok(()),
            }
        };
        let needs_region = matches!(place, TypePlace::Header | TypePlace::Body);
        match ty {
            Type::Ptr(inner, region) => {
                given(region)?;
                if needs_region && region.is_none() {
                    return err(span, format!("a pointer lives in a region, so its type names one, as in '{}@r'", ty));
                }
                let inner = self.resolve_regions(inner, span, place, region.as_deref())?;
                Ok(Type::Ptr(Box::new(inner), region.clone()))
            }
            Type::Slice(inner, region) => {
                given(region)?;
                if needs_region && region.is_none() {
                    return err(span, format!("a slice lives in a region, so its type names one, as in '{}@r'", ty));
                }
                let inner = self.resolve_regions(inner, span, place, region.as_deref())?;
                Ok(Type::Slice(Box::new(inner), region.clone()))
            }
            Type::Array(inner, count) => Ok(Type::Array(Box::new(self.resolve_regions(inner, span, place, inside)?), *count)),
            Type::Named(name, region) => {
                given(region)?;
                if !self.regional.contains(name) {
                    if let Some(region) = region
                        && inside != Some(region.as_str())
                    {
                        return err(self.place(span, &format!("@{}", region)), format!("'{}' holds no pointers, so it lives in no region; remove '@{}'", name, region));
                    }
                    return Ok(Type::named(name));
                }
                if needs_region && region.is_none() {
                    return err(self.place(span, name), format!("'{0}' holds pointers, so it lives in a region; name it, as in '{0}@r'", name));
                }
                Ok(ty.clone())
            }
            Type::Maybe(inner) => Ok(Type::Maybe(Box::new(self.resolve_regions(inner, span, place, inside)?))),
            Type::Or(value, error) => Ok(Type::Or(
                Box::new(self.resolve_regions(value, span, place, inside)?),
                Box::new(self.resolve_regions(error, span, place, inside)?),
            )),
            other => Ok(other.clone()),
        }
    }

    /// Checks that `name` is a region visible here; `allocating` also requires
    /// that `new` can allocate in it.
    fn visible_region(&self, name: &str, span: Span, allocating: bool) -> Result<()> {
        match self.lookup(name) {
            Some(info) if info.ty == Type::Region => {
                if allocating && !info.allocates {
                    return err(
                        span,
                        format!("this task can't allocate in '{0}', which it only names in its types; take it as a parameter, '{0}: region'", name),
                    );
                }
                Ok(())
            }
            Some(_) => err(span, format!("'{}' is a variable, not a region", name)),
            None => err(span, format!("unknown region '{0}'; declare it with 'region {0}'", name)),
        }
    }

    /// `ty`, whose regions are unsettled (a field's type, or the type after
    /// `new`), as it is in a value of region `region`: its pointers and
    /// regional shapes and kinds get `region`.
    fn in_region(&self, ty: &Type, region: &str) -> Type {
        in_region(ty, region, &self.regional)
    }

    /// A type written in the source must be a builtin type, a shape, or a kind.
    /// `unit` is only valid as a return type, which is checked separately, and
    /// as the value side of `unit or E`.
    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Unit => err(self.place(span, "unit"), "unit is only allowed as a return type"),
            Type::Region => err(self.place(span, "region"), "'region' is only the type of a task's parameter, as in 'task build(r: region)'"),
            // Only a task's header lists several types, and `check_header`
            // checks each of them on its own.
            Type::Results(_) => err(span, "several types, separated by commas, are only a task's results"),
            Type::Ptr(inner, _) => self.check_type(inner, span),
            Type::Array(inner, _) | Type::Slice(inner, _) => match inner.as_ref() {
                Type::Unit => {
                    let what = if matches!(ty, Type::Array(..)) { "an array" } else { "a slice" };
                    err(self.place(span, "unit"), format!("{} can't hold unit, which has no value", what))
                }
                _ => self.check_type(inner, span),
            },
            Type::Maybe(inner) => match inner.as_ref() {
                Type::Maybe(_) => err(span, "a maybe can't hold another maybe"),
                Type::Unit => err(self.place(span, "unit"), "a maybe can't hold unit, which has no value"),
                _ => self.check_type(inner, span),
            },
            Type::Or(value, error) => {
                if **value != Type::Unit {
                    self.check_type(value, span)?;
                }
                match error.as_ref() {
                    Type::Unit => return err(self.place(span, "or"), "the error type can't be unit"),
                    Type::Maybe(_) => return err(self.place(span, "or"), "the error type can't be a maybe"),
                    _ => self.check_type(error, span)?,
                }
                if value == error {
                    return err(self.place(span, "or"), format!("the error type must differ from the value type, as in '{} or ParseError'", value));
                }
                Ok(())
            }
            Type::Named(name, _) if !self.sees(name) || (!self.shapes.contains_key(name) && !self.kinds.contains_key(name)) => {
                err(self.place(span, name), format!("unknown type '{}'{}", name, self.elsewhere(name)))
            }
            _ => Ok(()),
        }
    }

    /// Rejects a shape or kind that contains itself by value, directly or
    /// through other types: its values would have infinite size. Containing a
    /// pointer to itself (`maybe ^T`) is fine. `acyclic` holds the types
    /// already known to be free of cycles. The search keeps its own stack, so
    /// a long chain of types can't overflow the compiler's.
    ///
    /// Each type found free of cycles is added to `order` after the types it
    /// contains by value.
    fn check_value_cycle(&self, root: &str, acyclic: &mut HashSet<String>, order: &mut Vec<String>) -> Result<()> {
        if acyclic.contains(root) {
            return Ok(());
        }
        // Each type being entered, with the types its fields contain by value
        // and how many of those have been followed.
        type Entered<'a> = (&'a str, Vec<(&'a str, Span)>, usize);
        let mut path: Vec<Entered> = vec![(root, self.contained_types(root), 0)];
        let mut on_path: HashSet<&str> = HashSet::from([root]);
        while let Some((_, targets, next)) = path.last_mut() {
            let Some(&(target, span)) = targets.get(*next) else {
                if let Some((name, _, _)) = path.pop() {
                    on_path.remove(name);
                    acyclic.insert(name.to_string());
                    order.push(name.to_string());
                }
                continue;
            };
            *next += 1;
            if on_path.contains(target) {
                let start = path.iter().position(|(name, _, _)| *name == target).unwrap_or_default();
                let chain: Vec<&str> = path.iter().skip(start).map(|(name, _, _)| *name).chain([target]).collect();
                return err(span, format!("'{}' contains itself by value ({}); use a pointer such as 'maybe ^{}'", target, chain.join(" -> "), target));
            }
            if !acyclic.contains(target) {
                on_path.insert(target);
                path.push((target, self.contained_types(target), 0));
            }
        }
        Ok(())
    }

    /// The types a shape's fields or a kind's payloads contain by value, each
    /// with the position of the field's type.
    fn contained_types(&self, name: &str) -> Vec<(&str, Span)> {
        let fields: Vec<&Field> = match (self.shapes.get(name), self.kinds.get(name)) {
            (Some(shape), _) => shape.fields.iter().collect(),
            (None, Some(kind)) => kind.variants.iter().flat_map(|v| &v.payload).collect(),
            (None, None) => Vec::new(),
        };
        fields.into_iter().flat_map(|field| contained_by_value(&field.ty).into_iter().map(|target| (target, field.ty_span))).collect()
    }

    /// Rejects a shape or kind that could never be created. A pointer can't be
    /// `none`, so a shape whose field points to its own type (directly or
    /// through other types) would need one to exist before the first is made.
    /// A kind can be created if any of its variants can.
    fn check_constructible(&self, items: &[Item]) -> Result<()> {
        // The items whose fields name each type: when a type turns out to be
        // creatable, only those need another look.
        let mut users: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            let fields: Vec<&Field> = match item {
                Item::Shape(s) => s.fields.iter().collect(),
                Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).collect(),
                Item::Task(_) => continue,
            };
            for field in fields {
                for name in named_types(&field.ty) {
                    users.entry(name).or_default().push(i);
                }
            }
        }
        let mut can: HashSet<&str> = HashSet::new();
        let mut pending: Vec<usize> = (0..items.len()).collect();
        while let Some(i) = pending.pop() {
            let (name, ok) = match items.get(i) {
                Some(Item::Shape(s)) => (s.name.as_str(), s.fields.iter().all(|f| constructible(&f.ty, &can))),
                Some(Item::Kind(k)) => (k.name.as_str(), k.variants.iter().any(|v| v.payload.iter().all(|f| constructible(&f.ty, &can)))),
                _ => continue,
            };
            if ok && can.insert(name) {
                pending.extend(users.get(name).into_iter().flatten().copied());
            }
        }
        for item in items {
            match item {
                Item::Shape(s) if !can.contains(s.name.as_str()) => {
                    if let Some(field) = s.fields.iter().find(|f| !constructible(&f.ty, &can)) {
                        return err(
                            field.ty_span,
                            format!(
                                "'{}' can never be created: field '{}' needs an existing {}; make it 'maybe {}'",
                                s.name, field.name, field.ty, field.ty
                            ),
                        );
                    }
                }
                Item::Kind(k) if !can.contains(k.name.as_str()) => {
                    return err(
                        k.span,
                        format!("'{}' can never be created: every variant needs an existing value that can't be created either; use 'maybe' for a value that can be missing", k.name),
                    );
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn check_task(&mut self, task: &mut TaskDef) -> Result<()> {
        for param in &task.params {
            self.check_size(&param.ty, param.ty_span)?;
        }
        for (i, result) in task.ret.results().iter().enumerate() {
            self.check_size(result, task.result_span(i))?;
        }
        self.current_ret = Some(task.ret.clone());
        // Parameters and the task body share one scope, as in C.
        self.scopes = vec![HashMap::new()];
        for param in &task.params {
            if param.ty == Type::Region {
                self.define_region(&param.name, param.span, true)?;
            } else {
                self.define(&param.name, param.ty.clone(), false, param.span)?;
            }
        }
        // The regions the header only names in its types.
        for param in &task.params {
            for region in param.ty.regions() {
                if self.lookup(region).is_none() {
                    self.define_region(region, param.ty_span, false)?;
                }
            }
        }
        // Named results are variables of the task, declared by its header.
        self.result_names = task.result_names().map(|(name, _)| name.clone()).collect();
        self.unset = self.result_names.iter().cloned().collect();
        for (result, ty) in task.results.iter().zip(task.ret.results()) {
            if let Some((name, at)) = &result.name {
                self.define_result(name, ty.clone(), *at)?;
            }
        }
        self.check_block(&mut task.body)?;
        self.scopes.clear();
        self.current_ret = None;
        if self.result_names.is_empty() {
            if sends_value(&task.ret) && !always_sends(&task.body) {
                return err(task.span, format!("task '{}' can finish without sending a value", task.name));
            }
        } else if !always_sends(&task.body)
            && let Some(name) = self.first_unset()
        {
            // Reaching the end sends the results, so they must all be set.
            return err(task.span, format!("task '{}' can finish without setting '{}'", task.name, name));
        }
        self.result_names.clear();
        self.unset.clear();
        Ok(())
    }

    /// Checks `block` as a path of its own, and returns the results it leaves
    /// unset, or `None` when every path through it leaves the task, so nothing
    /// after it runs on this path. `entry` is the state the block starts from.
    fn branch_state(&mut self, block: &Block, entry: &HashSet<String>) -> Option<HashSet<String>> {
        let after = std::mem::replace(&mut self.unset, entry.clone());
        (!always_sends(block)).then_some(after)
    }

    /// The state after branches that each start from `entry`: a result is set
    /// only where every branch that runs on sets it. Where no branch runs on,
    /// what follows is unreachable, and `entry` is kept for checking it.
    fn join_branches(&mut self, entry: HashSet<String>, branches: Vec<Option<HashSet<String>>>) {
        let mut after: Option<HashSet<String>> = None;
        for branch in branches.into_iter().flatten() {
            after.get_or_insert_with(HashSet::new).extend(branch);
        }
        self.unset = after.unwrap_or(entry);
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Checks `block` in a new nested scope.
    fn check_scoped_block(&mut self, block: &mut Block) -> Result<()> {
        self.push_scope();
        let result = self.check_block(block);
        self.pop_scope();
        result
    }

    /// Checks `block` in a new nested scope that first declares `bindings`
    /// (from a pattern), each immutable. `_` declares nothing.
    fn check_block_with(&mut self, bindings: Vec<(String, Span, Type)>, block: &mut Block) -> Result<()> {
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _)| name != DISCARD)
            .try_for_each(|(name, span, ty)| self.define(&name, ty, false, span))
            .and_then(|()| self.check_block(block));
        self.pop_scope();
        result
    }

    /// Declares a variable in the innermost scope. Vela has no shadowing: a
    /// name cannot be declared while another variable of that name is visible.
    /// (Sibling blocks may reuse names, since neither is visible to the other.)
    fn define(&mut self, name: &str, ty: Type, mutable: bool, span: Span) -> Result<()> {
        // A variable named like a variant would make a bare `Empty` ambiguous.
        if let Some(info) = self.variant_in_scope(name) {
            let at = info.variant.span;
            if at.file != span.file {
                return err(span, format!("'{}' is already the name of a variant of kind '{}'", name, info.kind));
            }
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        // Nor like a kind, which `Kind.Variant` would read as.
        if self.kinds.contains_key(&self.in_module(name)) {
            return err(span, format!("'{0}' is the name of a kind, so '{0}.x' would name its variant; choose another name", name));
        }
        // Nor may it take a module's name, which `name.x` would read as.
        if let Some(at) = self.module_used(name) {
            return err(span, format!("'{}' is the name of a module this file includes, at {}:{}", name, at.line, at.col));
        }
        if let Some(existing) = self.lookup(name) {
            let at = existing.span;
            let what = match existing {
                _ if existing.ty == Type::Region => "the name of a region",
                _ if existing.result => "a result of this task, declared",
                _ => "defined",
            };
            return err(span, format!("'{}' is already {} at {}:{}", name, what, at.line, at.col));
        }
        let Some(scope) = self.scopes.last_mut() else {
            return Err(Error::internal(format!("'{}' defined outside of a task", name)));
        };
        let allocates = ty == Type::Region;
        scope.insert(name.to_string(), VarInfo { ty, mutable, span, allocates, result: false });
        Ok(())
    }

    /// Declares a named result, which reads like a variable and is set only by
    /// `total <- value`.
    fn define_result(&mut self, name: &str, ty: Type, span: Span) -> Result<()> {
        self.define(name, ty, false, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.get_mut(name)) {
            info.result = true;
        }
        Ok(())
    }

    /// The first result, in the order the header names them, that isn't set on
    /// this path.
    fn first_unset(&self) -> Option<&str> {
        self.result_names.iter().find(|name| self.unset.contains(*name)).map(String::as_str)
    }

    /// Declares a region in the innermost scope; `allocates` tells whether
    /// `new` can allocate in it.
    fn define_region(&mut self, name: &str, span: Span, allocates: bool) -> Result<()> {
        self.define(name, Type::Region, false, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.get_mut(name)) {
            info.allocates = allocates;
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info);
            }
        }
        None
    }

    /// The variable called `name`, which must not be a region.
    fn lookup_or_err(&self, name: &str, span: Span) -> Result<&VarInfo> {
        if let Some(info) = self.lookup(name)
            && info.ty == Type::Region
        {
            return err(span, format!("'{0}' is a region, not a value; allocate in it with 'new T(…) at {0}'", name));
        }
        self.lookup(name).ok_or_else(|| {
            if self.sees(name) && self.variants.contains_key(name) {
                Error::at(span, format!("'{}' is a variant, not a variable", name))
            } else if self.module_used(name).is_some() {
                Error::at(span, format!("'{0}' is a module, not a variable; name something in it, as in '{0}.name'", name))
            } else if self.kinds.contains_key(&self.full_name(name)) {
                Error::at(span, format!("'{0}' is a kind, not a variable; name one of its variants, as in '{0}.Variant'", name))
            } else if let Some(shape) = self.shapes.get(&self.full_name(name)) {
                // Most likely a shape literal the parser read as a plan B.
                let example = match shape.fields.first() {
                    Some(field) => format!("{} {{ {}: … }}", name, field.name),
                    None => format!("{} {{}}", name),
                };
                Error::at(span, format!("'{}' is a shape, not a variable; a literal names its fields on one line, as in '{}'", name, example))
            } else {
                let hint = match self.module_elsewhere(name) {
                    hint if hint.is_empty() => self.elsewhere(name),
                    hint => hint,
                };
                Error::at(span, format!("unknown variable '{}'{}", name, hint))
            }
        })
    }

    fn check_block(&mut self, block: &mut [Stmt]) -> Result<()> {
        for stmt in block {
            self.check_stmt(stmt)?;
        }
        Ok(())
    }

    fn check_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(e) => {
                // `fail e` names a plan B's error only on its first line.
                if let ExprKind::Fail(inner) = &e.kind
                    && let ExprKind::Var(name) = &inner.kind
                    && self.lookup(name).is_none()
                {
                    return err(e.span, format!("'fail {0}' names the error only on the first line of a plan B; to send an error, write '<- fail {0}'", name));
                }
                self.check_expr(e)?;
                // Of all expressions, only a call can have an effect, and so
                // can a plan B on a call, or one that gives no value.
                match &e.kind {
                    ExprKind::Call(name, _) if matches!(e.ty, Some(Type::Or(..))) => err(e.span, ignored_error(name)),
                    ExprKind::Call(..) => Ok(()),
                    ExprKind::PlanB { value, .. } if e.ty == Some(Type::Unit) || matches!(value.kind, ExprKind::Call(..)) => Ok(()),
                    ExprKind::Binary(BinaryOp::Eq, ..) => err(e.span, "the result of '==' is unused; use '=' to assign"),
                    _ => err(e.span, "this value is unused; only a task call can be used as a statement"),
                }
            }
            StmtKind::Binding { names, mutable, init } => {
                for bound in names.iter_mut() {
                    if let Some(annotated) = &mut bound.ty {
                        *annotated = self.resolve_type(annotated, bound.ty_span.unwrap_or(bound.span), TypePlace::Body)?;
                    }
                }
                let given = self.value_types(init, names.len(), span)?;
                // A single call sending several values gives them all, and
                // those values are taken as they are.
                let forwarded = init.len() < names.len();
                let list_span = init.first().map_or(span, |value| value.span);
                let mut declared = Vec::new();
                for i in 0..names.len() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    let Some((name, at, annotated)) = names.get(i).map(|b| (b.name.clone(), b.span, b.ty.clone())) else { continue };
                    let value_span = if forwarded { list_span } else { init.get(i).map_or(span, |value| value.span) };
                    let ty = match annotated {
                        Some(annotated) => {
                            match init.get_mut(i).filter(|_| !forwarded) {
                                Some(value) => self.coerce(value, &annotated)?,
                                None => fits_or_err(&actual, &annotated, value_span)?,
                            }
                            annotated
                        }
                        None if name == DISCARD => actual,
                        None => self.inferred_type(&name, &actual, value_span)?,
                    };
                    if name == DISCARD && matches!(ty, Type::Or(..)) {
                        return err(value_span, dropped_error(&ty));
                    }
                    if let Some(bound) = names.get_mut(i) {
                        bound.ty = Some(ty.clone());
                    }
                    declared.push((name, at, ty));
                }
                for (name, at, ty) in declared {
                    if name != DISCARD {
                        self.define(&name, ty, *mutable, at)?;
                    }
                }
                Ok(())
            }
            StmtKind::Assign(targets, values) => {
                let mut places = Vec::new();
                let mut assigned: HashMap<String, Span> = HashMap::new();
                for target in targets.iter_mut() {
                    places.push(match target {
                        Target::Discard(_) => None,
                        Target::Place(place) => {
                            // Each place takes one value, so a variable
                            // assigned twice would quietly keep the last.
                            if let ExprKind::Var(name) = &place.kind
                                && let Some(at) = assigned.insert(name.clone(), place.span)
                            {
                                return err(place.span, format!("'{}' is already assigned at {}:{}", name, at.line, at.col));
                            }
                            Some(self.check_assign_target(place)?)
                        }
                    });
                }
                let given = self.value_types(values, targets.len(), span)?;
                let forwarded = values.len() < targets.len();
                let list_span = values.first().map_or(span, |value| value.span);
                for (i, place) in places.into_iter().enumerate() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    let value_span = if forwarded { list_span } else { values.get(i).map_or(span, |value| value.span) };
                    let Some(expected) = place else {
                        if matches!(actual, Type::Or(..)) {
                            return err(value_span, dropped_error(&actual));
                        }
                        continue;
                    };
                    match values.get_mut(i).filter(|_| !forwarded) {
                        Some(value) => self.coerce(value, &expected)?,
                        None => fits_or_err(&actual, &expected, value_span)?,
                    }
                }
                Ok(())
            }
            StmtKind::SetResults(names, values) => self.check_set_results(span, names, values),
            StmtKind::When(cond, then_block, else_block) => {
                let entry = self.unset.clone();
                self.check_condition(cond, then_block)?;
                let after_then = self.branch_state(then_block, &entry);
                let after_else = match else_block {
                    Some(else_b) => {
                        self.check_scoped_block(else_b)?;
                        self.branch_state(else_b, &entry)
                    }
                    // Without an `else`, not running the block is a path too.
                    None => Some(entry.clone()),
                };
                self.join_branches(entry, vec![after_then, after_else]);
                Ok(())
            }
            StmtKind::Loop(cond, body) => {
                // A loop's body might not run, so it sets nothing for certain.
                let entry = self.unset.clone();
                self.check_condition(cond, body)?;
                self.unset = entry;
                Ok(())
            }
            StmtKind::Each(name, range, body) => {
                // A range is only valid here, so it is checked here rather than
                // as an ordinary expression.
                let item = if let ExprKind::Range(start, end) = &mut range.kind {
                    for bound in [start, end] {
                        let bty = self.check_expr(bound)?;
                        self.expect_type(&bty, &Type::Int, bound.span)?;
                    }
                    range.ty = Some(Type::Range(Box::new(Type::Int)));
                    Type::Int
                } else {
                    match self.check_expr(range)? {
                        Type::Array(items, _) | Type::Slice(items, _) if unsettled(&items) => {
                            return err(range.span, format!("cannot tell the type of '{}' from none or 'fail …'; give the array a type first", name));
                        }
                        Type::Array(items, _) | Type::Slice(items, _) => *items,
                        other => {
                            return err(
                                range.span,
                                format!("each goes through a range, such as 0..n, or the items of an array or slice, but this value is {}{}", other, extract_hint(&[&other])),
                            );
                        }
                    }
                };
                // The loop variable and the body share one scope.
                let entry = self.unset.clone();
                self.push_scope();
                let result = self.define(name, item, false, span)
                    .and_then(|()| self.check_block(body));
                self.pop_scope();
                self.unset = entry;
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases),
            StmtKind::Send(values) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                let expected = ret.results();
                if values.is_empty() {
                    if !self.result_names.is_empty() {
                        // A bare `<-` sends the results as they are.
                        return match self.first_unset() {
                            Some(name) => err(span, format!("'{}' might not be set here", name)),
                            None => Ok(()),
                        };
                    }
                    return match (sends_value(&ret), expected.len()) {
                        (false, _) => Ok(()),
                        (true, 1) => err(span, "expected a value after '<-'"),
                        (true, n) => err(span, format!("expected {} after '<-', one for each result", count(n, "value"))),
                    };
                }
                let first = values.first().map_or(span, |value| value.span);
                if expected.is_empty() {
                    return err(first, "this task has no return type, so '<-' cannot take a value");
                }
                // One value for one result, which may become a `maybe`, a
                // `T or E`, or the error of one.
                if let ([e], [ret]) = (values.as_mut_slice(), expected) {
                    let ret = ret.clone();
                    let ety = self.check_expr(e)?;
                    if !sends_value(&ret) && !matches!(ety, Type::Fail(_)) && !fits_regions(&ety, &ret) {
                        return err(e.span, "this task sends no value when it succeeds; write a bare '<-', or '<- fail …' to send an error");
                    }
                    return self.coerce(e, &ret);
                }
                let expected = expected.to_vec();
                let given = self.value_types(values, expected.len(), span)?;
                let forwarded = values.len() < expected.len();
                for (i, want) in expected.iter().enumerate() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    match values.get_mut(i).filter(|_| !forwarded) {
                        Some(value) => self.coerce(value, want)?,
                        None => fits_or_err(&actual, want, first)?,
                    }
                }
                Ok(())
            }
            StmtKind::Region(name) => self.define_region(name, span, true),
        }
    }

    /// Checks a `when` or `loop` condition and the block it guards, where the
    /// names an `is` pattern binds are visible.
    fn check_condition(&mut self, cond: &mut Condition, body: &mut Block) -> Result<()> {
        let ty = self.check_expr(&mut cond.value)?;
        let Some(pattern) = &cond.pattern else {
            self.expect_type(&ty, &Type::Bool, cond.value.span)?;
            return self.check_scoped_block(body);
        };
        if matches!(pattern.kind, PatternKind::Any) {
            return err(pattern.span, "'_' matches anything, so this condition would always hold");
        }
        let bindings = self.pattern_bindings(&ty, pattern, false)?;
        self.check_block_with(bindings, body)
    }

    /// Checks that `pattern` can match a value of type `subject`, and returns
    /// the names it binds with their types. `in_case` words messages for a
    /// `case` rather than an `is`.
    fn pattern_bindings(&self, subject: &Type, pattern: &Pattern, in_case: bool) -> Result<Vec<(String, Span, Type)>> {
        let span = pattern.span;
        let bound = |(name, at): &Binding, ty: &Type| vec![(name.clone(), *at, ty.clone())];
        // `fail TooBig` or `{Red}`: a pattern names the whole error or value.
        if let PatternKind::Value((name, at)) | PatternKind::Fail((name, at)) = &pattern.kind
            && self.variant_in_scope(name).is_some()
        {
            let (keyword, whole) = if matches!(pattern.kind, PatternKind::Fail(_)) { ("fail e", "error") } else { ("{value}", "value") };
            return err(*at, format!("'{}' is a variant, but '{}' names the whole {}; give it a name, then match on it", name, keyword, whole));
        }
        match (&pattern.kind, subject) {
            (PatternKind::Any | PatternKind::None, Type::Maybe(_)) => Ok(Vec::new()),
            (PatternKind::Any, _) => Ok(Vec::new()),
            (PatternKind::Variant { kind, tag, binds }, Type::Named(kind_name, region)) if self.kinds.contains_key(kind_name) => {
                if let Some((written, at)) = kind
                    && written != kind_name
                {
                    return err(*at, format!("this pattern names kind '{}', but the value's kind is '{}'", written, kind_name));
                }
                let variant = self.kinds.get(kind_name)
                    .and_then(|kind| kind.variants.iter().find(|v| v.tag == *tag))
                    .ok_or_else(|| Error::at(span, format!("kind '{}' has no variant '{}'", kind_name, tag)))?;
                if binds.len() != variant.payload.len() {
                    let what = if in_case { format!("case '{}'", tag) } else { format!("'{}'", tag) };
                    return err(span, format!("{} expects {}, got {}", what, count(variant.payload.len(), "binding"), binds.len()));
                }
                Ok(binds.iter().zip(&variant.payload).map(|((name, at), field)| (name.clone(), *at, self.field_in(&field.ty, region))).collect())
            }
            (PatternKind::Variant { tag, .. }, other) => {
                if self.variant_in_scope(tag).is_some() {
                    err(span, format!("'{}' is a variant, but this value is {}", tag, other))
                } else {
                    err(span, format!("unknown variant '{}'", tag))
                }
            }
            (PatternKind::Value(bind), Type::Maybe(inner)) => Ok(bound(bind, inner)),
            (PatternKind::Value(bind), Type::Or(value, _)) => {
                if **value == Type::Unit && bind.0 != DISCARD {
                    return err(bind.1, "this success has no value to name; write '{_}'");
                }
                Ok(bound(bind, value))
            }
            (PatternKind::Fail(bind), Type::Or(_, error)) => Ok(bound(bind, error)),
            (PatternKind::Value(_), other) => {
                err(span, format!("'{{…}}' matches the value in a maybe or a 'T or E', but this value is {}", other))
            }
            (PatternKind::None, Type::None) => err(span, "this value is always none, so there is nothing to match"),
            (PatternKind::None, other) => err(span, format!("'none' matches an empty maybe, but this value is {}", other)),
            (PatternKind::Fail(_), other) => {
                err(span, format!("'fail …' matches the error of a 'T or E', but this value is {}", other))
            }
        }
    }

    /// Checks the left side of an assignment and returns its type.
    ///
    /// Assignable places are: a `hold` variable, a field of an assignable
    /// place, and anything behind a pointer (`p^`, `p^.x`). Writing through a
    /// pointer is allowed even when the pointer itself is a `bind`, because
    /// the binding holds the address, not the pointed-to value.
    fn check_assign_target(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, span)?;
                if info.result {
                    return err(span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(span, format!("cannot assign to immutable variable '{}'", name));
                }
                let ty = info.ty.clone();
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Field(base, field) => {
                let base_ty = self.check_field_base(base)?;
                let ty = self.field_type(&base_ty, field, span)?;
                match base_ty {
                    Type::Array(..) => return err(span, "'len' can't be assigned: an array's length is part of its type"),
                    Type::Slice(..) => return err(span, "'len' can't be assigned: a slice's length changes only by slicing it, as in 'xs = xs[0..2]'"),
                    _ => {}
                }
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Deref(_) => self.check_expr(expr),
            ExprKind::Index(base, index) => {
                if matches!(index.kind, ExprKind::Range(..)) {
                    return err(span, "a slice of values can't be assigned; assign its items one at a time, as in 'xs[i] = v'");
                }
                let base_ty = self.check_item_base(base)?;
                let ty = self.check_index(base, &base_ty, index, span)?;
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            _ => err(span, "only a variable, a field, an item of an array or slice, or the value a pointer points to (p^) can be assigned"),
        }
    }

    /// Checks the array or slice whose item is assigned, and returns its type.
    /// An array holds its items, so it must be an assignable place itself; a
    /// slice's items are in a region, behind a pointer, so they can always be
    /// written, as through `p^`.
    fn check_item_base(&mut self, base: &mut Expr) -> Result<Type> {
        if let ExprKind::Var(name) = &base.kind {
            let info = self.lookup_or_err(name, base.span)?;
            if matches!(info.ty, Type::Array(..)) {
                if info.result {
                    return err(base.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(base.span, format!("cannot assign to an item of immutable variable '{}'", name));
                }
            }
            return self.check_expr(base);
        }
        let ty = self.check_expr(base)?;
        if matches!(ty, Type::Array(..)) {
            self.assignable_array(base)?;
        }
        Ok(ty)
    }

    /// Rejects an array, already checked, whose item can't be assigned: one in a
    /// `bind` variable, a result, or a temporary value.
    fn assignable_array(&self, place: &Expr) -> Result<()> {
        match &place.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, place.span)?;
                if info.result {
                    return err(place.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(place.span, format!("cannot assign to an item of immutable variable '{}'", name));
                }
                Ok(())
            }
            ExprKind::Deref(_) => Ok(()),
            ExprKind::Field(base, _) => self.assignable_array(base),
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..))) => Ok(()),
            ExprKind::Index(base, _) => self.assignable_array(base),
            _ => err(place.span, "cannot assign to an item of a temporary value"),
        }
    }

    /// Checks the base of an assigned field, which must itself be an
    /// assignable place.
    fn check_field_base(&mut self, base: &mut Expr) -> Result<Type> {
        let span = base.span;
        match &base.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, span)?;
                // Only a shape has fields to assign; for anything else (a pointer,
                // a kind) the field error from `field_type` is the useful one.
                let is_shape = matches!(&info.ty, Type::Named(ty, _) if self.shapes.contains_key(ty));
                if is_shape && !info.mutable {
                    return err(span, format!("cannot assign to a field of immutable variable '{}'", name));
                }
                self.check_expr(base)
            }
            ExprKind::Field(..) | ExprKind::Deref(_) | ExprKind::Index(..) => self.check_assign_target(base),
            _ => err(span, "cannot assign to a field of a temporary value"),
        }
    }

    fn check_match(&mut self, span: Span, expr: &mut Expr, cases: &mut [Case]) -> Result<()> {
        let ety = self.check_expr(expr)?;
        // The cases that together cover every value.
        let needed: Vec<String> = match &ety {
            Type::Named(name, _) => match self.kinds.get(name) {
                Some(kind) => kind.variants.iter().map(|v| v.tag.clone()).collect(),
                None => return err(expr.span, format!("'match' needs a kind, a maybe, or a 'T or E', but '{}' is a shape", name)),
            },
            Type::Maybe(_) => vec!["{…}".to_string(), "none".to_string()],
            Type::Or(..) => vec!["{…}".to_string(), "fail …".to_string()],
            other => return err(expr.span, format!("match requires a kind, a maybe, or a 'T or E' value, got {}", other)),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut matched_all = false;
        // Every case is a path of its own, and a result is set after the match
        // only where every case that runs on sets it.
        let entry = self.unset.clone();
        let mut branches = Vec::new();
        for case in cases {
            if matched_all {
                return err(case.pattern.span, "this case comes after '_', which already matches everything");
            }
            let bindings = self.pattern_bindings(&ety, &case.pattern, true)?;
            let label = case.pattern.label();
            if matches!(case.pattern.kind, PatternKind::Any) {
                if needed.iter().all(|n| seen.contains(n)) {
                    return err(case.pattern.span, "every case is already covered, so '_' never matches");
                }
                matched_all = true;
            } else if seen.contains(&label) {
                return err(case.pattern.span, format!("duplicate case '{}'", label));
            }
            seen.push(label);
            self.check_block_with(bindings, &mut case.body)?;
            branches.push(self.branch_state(&case.body, &entry));
        }
        // A value the cases don't cover would leave the match without setting
        // anything, but then a case is missing, which is the error below.
        self.join_branches(entry, branches);
        if !matched_all
            && let Some(missing) = needed.iter().find(|n| !seen.contains(n))
        {
            return err(span, format!("missing case '{}' in match", missing));
        }
        Ok(())
    }

    /// Checks an expression, records its type on the node, and returns it. A
    /// call that sends several values is rejected here: its values are
    /// separate, so only a list can receive them (see `value_types`).
    fn check_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let ty = self.check_expr_multi(expr)?;
        let Type::Results(types) = &ty else { return Ok(ty) };
        let what = match &expr.kind {
            ExprKind::Call(name, _) => format!("'{}'", name),
            _ => "this call".to_string(),
        };
        err(
            expr.span,
            format!("{} sends {} values; receive them with 'bind {} = …'", what, types.len(), example_names(types.len())),
        )
    }

    /// `check_expr`, which also allows a call that sends several values.
    fn check_expr_multi(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
        self.resolve_empty_plan_b(expr);
        let ty = self.infer_expr(expr)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    /// Checks the values of a list — a send, or the right side of a binding or
    /// an assignment — and returns the type of each. A single call that sends
    /// `wanted` values gives them all; otherwise every value gives its own, and
    /// there must be one per name.
    fn value_types(&mut self, values: &mut [Expr], wanted: usize, span: Span) -> Result<Vec<Type>> {
        let mut given = Vec::new();
        if let ([only], false) = (&mut *values, wanted == 1) {
            let ty = self.check_expr_multi(only)?;
            match ty {
                Type::Results(types) => given = types,
                other => given.push(other),
            }
        } else {
            for value in values.iter_mut() {
                given.push(self.check_expr(value)?);
            }
        }
        if given.len() != wanted {
            let at = values.get(wanted).map_or(span, |extra| extra.span);
            return err(at, format!("expected {}, got {}", count(wanted, "value"), given.len()));
        }
        Ok(given)
    }

    /// Checks `total <- value`, which sets named results and keeps going. The
    /// names may be any of the task's results, in any order, each once.
    fn check_set_results(&mut self, span: Span, names: &[Binding], values: &mut [Expr]) -> Result<()> {
        let mut expected = Vec::new();
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for (name, at) in names {
            if let Some(before) = seen.insert(name, *at) {
                return err(*at, format!("'{}' is already set at {}:{}", name, before.line, before.col));
            }
            let Some(info) = self.lookup(name).filter(|info| info.result) else {
                return err(*at, self.not_a_result(name));
            };
            expected.push(info.ty.clone());
        }
        let given = self.value_types(values, expected.len(), span)?;
        let forwarded = values.len() < expected.len();
        let list_span = values.first().map_or(span, |value| value.span);
        for (i, want) in expected.iter().enumerate() {
            let Some(actual) = given.get(i).cloned() else { continue };
            match values.get_mut(i).filter(|_| !forwarded) {
                Some(value) => self.coerce(value, want)?,
                None => fits_or_err(&actual, want, list_span)?,
            }
        }
        // Set, so the results can be read from here on.
        for (name, _) in names {
            self.unset.remove(name);
        }
        Ok(())
    }

    /// Why `name <- value` doesn't name a result of this task.
    fn not_a_result(&self, name: &str) -> String {
        if self.lookup(name).is_some() {
            return format!("'{0}' isn't a result of this task; assign it with '{0} = …'", name);
        }
        match self.result_names.as_slice() {
            [] => format!("'{}' isn't a result of this task, which names none; a header names them, as in '-> total: int'", name),
            names => format!("'{}' isn't a result of this task; its results are {}", name, names.join(", ")),
        }
    }

    /// The type a `bind` or `hold` without an annotation gives `name`, from the
    /// type of its value.
    fn inferred_type(&self, name: &str, actual: &Type, at: Span) -> Result<Type> {
        match actual {
            Type::None => err(at, format!("cannot infer the type of '{}' from none; give it a type, such as '{}: maybe int'", name, name)),
            Type::Fail(error) => err(
                at,
                format!("cannot infer the type of '{}' from 'fail …'; give it a type, such as '{}: int or {}'", name, name, error),
            ),
            Type::Unit => err(at, "this expression has no value (its type is unit)"),
            other if unsettled(other) => err(
                at,
                format!("cannot infer the type of '{0}' from none or 'fail …' in its value; give it a type, such as '{0}: {1}'", name, settled_example(other)),
            ),
            other if other.regions().contains(&ANY_REGION) => {
                let example = other.map_regions(&mut |region| Some(if region == ANY_REGION { "r" } else { region }.to_string()));
                err(
                    at,
                    format!(
                        "cannot tell which region '{0}' lives in, since its value holds no pointers yet; give it a type, such as '{0}: {1}'",
                        name, example
                    ),
                )
            }
            other => Ok(other.clone()),
        }
    }

    /// Rewrites a variant used as a value, `Empty`, or called, `Circle(1.0)`,
    /// into a `Construct` node. Variables can't share a variant's name, so a
    /// bare name is never ambiguous.
    fn resolve_constructor(&self, expr: &mut Expr) {
        let variant = |name: &str| {
            self.variants.get(name).filter(|_| self.sees(name)).map(|info| (info.kind.clone(), info.variant.tag.clone()))
        };
        let resolved = match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => variant(name).map(|(kind, tag)| (kind, tag, Vec::new())),
            ExprKind::Call(name, args) => variant(name).map(|(kind, tag)| (kind, tag, std::mem::take(args))),
            _ => None,
        };
        if let Some((kind, tag, args)) = resolved {
            expr.kind = ExprKind::Construct { kind, tag, args };
        }
    }

    /// Rewrites `name {}` into an empty plan B when `name` is a variable rather
    /// than a shape: the parser reads both as a shape literal.
    fn resolve_empty_plan_b(&self, expr: &mut Expr) {
        let ExprKind::StructInit(name, fields) = &mut expr.kind else { return };
        if !fields.is_empty() || (self.sees(name) && self.shapes.contains_key(name.as_str())) || self.lookup(name).is_none() {
            return;
        }
        let value = Expr::new(ExprKind::Var(std::mem::take(name)), expr.span);
        expr.kind = ExprKind::PlanB { value: Box::new(value), error: None, body: Vec::new() };
    }

    fn infer_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text),
            ExprKind::NoneLit => Ok(Type::None),
            ExprKind::Var(name) => {
                let ty = self.lookup_or_err(name, span)?.ty.clone();
                if self.unset.contains(name.as_str()) {
                    return err(span, format!("'{}' might not be set here", name));
                }
                Ok(ty)
            }
            ExprKind::Field(base, field) => {
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field, span)
            }
            ExprKind::Index(base, index) => {
                let base_ty = self.check_expr(base)?;
                self.check_index(base, &base_ty, index, span)
            }
            ExprKind::Array(values) => self.check_array(values, span),
            ExprKind::Repeat { value, count } => self.check_repeat(value, count, span),
            ExprKind::Call(name, args) => {
                if name == "show" {
                    let [arg] = args.as_mut_slice() else {
                        return err(span, "show expects 1 argument");
                    };
                    let ty = self.check_expr(arg)?;
                    let shown = match &ty {
                        Type::Maybe(inner) => inner.as_ref(),
                        other => other,
                    };
                    if !matches!(shown, Type::Int | Type::Float | Type::Bool | Type::Text) {
                        let hint = match &ty {
                            Type::Ptr(..) => "; to show the value it points to, write 'p^'".to_string(),
                            Type::Array(..) | Type::Slice(..) => "; show its items one at a time, as in 'each v in xs'".to_string(),
                            Type::Or(..) => extract_hint(&[&ty]),
                            _ => String::new(),
                        };
                        return err(arg.span, format!("show expects int, float, bool, or text{}", hint));
                    }
                    return Ok(Type::Unit);
                }
                let (params, ret) = self.tasks.get(name.as_str()).filter(|_| self.sees(name))
                    .ok_or_else(|| {
                        // `send (x)` written the old way reads as a call.
                        let hint = match name.as_str() {
                            "send" => "; a task sends its result with '<-'".to_string(),
                            "free" => "; Vela has no 'free': memory in a region is freed when the block declaring the region closes".to_string(),
                            _ if self.module_used(name).is_some() => format!("; '{0}' is a module, so name a task in it, as in '{0}.name(…)'", name),
                            _ => self.elsewhere(name),
                        };
                        Error::at(span, format!("unknown task '{}'{}", name, hint))
                    })?
                    .clone();
                if args.len() != params.len() {
                    return err(span, format!("task '{}' expects {}, got {}", name, count(params.len(), "argument"), args.len()));
                }
                self.check_call_args(name, args, &params, &ret)
            }
            ExprKind::Unary(op, e) => self.check_unary(*op, e, span),
            ExprKind::Binary(op, l, r) => self.check_binary(*op, l, r, span),
            ExprKind::Construct { kind, tag, args } => {
                let variant = self.kinds.get(kind.as_str())
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .cloned()
                    .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
                if args.len() != variant.payload.len() {
                    return err(span, format!("variant '{}' expects {}, got {}", tag, count(variant.payload.len(), "argument"), args.len()));
                }
                let mut values: Vec<&mut Expr> = args.iter_mut().collect();
                let region = self.check_fields_of_value(&mut values, &variant.payload)?;
                Ok(Type::Named(kind.clone(), self.regional.contains(kind.as_str()).then_some(region)))
            }
            ExprKind::New { ty, ty_span, value, region, region_span } => {
                **ty = self.resolve_type(ty, *ty_span, TypePlace::New)?;
                // `^int or E` means `(^int) or E`, so no written type could hold the pointer.
                if matches!(**ty, Type::Or(..)) {
                    return err(*ty_span, format!("'new' can't make a pointer to '{}'; extract the value first, or keep it in a shape's field", ty));
                }
                self.visible_region(region, *region_span, true)?;
                // `new [T](…) at r` makes the slice itself, with its items in `r`.
                if let Type::Slice(items, _) = &**ty {
                    let items = self.in_region(items, region);
                    let slice = Type::Slice(Box::new(items.clone()), Some(region.clone()));
                    self.check_slice_source(value, &items, &slice)?;
                    return Ok(slice);
                }
                let pointee = self.in_region(ty, region);
                self.check_expr(value)?;
                self.coerce(value, &pointee)?;
                Ok(Type::Ptr(Box::new(pointee), Some(region.clone())))
            }
            ExprKind::Deref(e) => {
                let ty = self.check_expr(e)?;
                if let Type::Maybe(inner) = &ty
                    && inner.ptr_inner().is_some()
                {
                    return err(span, format!("this pointer is a {}, which may be none; extract it first, as in 'when p is {{q}}' or 'p {{ … }}'", ty));
                }
                if let Type::Or(value, _) = &ty
                    && value.ptr_inner().is_some()
                {
                    return err(span, format!("this pointer is a {}, which may be an error; extract it first, as in 'p {{ … }}' or 'match'", ty));
                }
                let hint = if matches!(ty, Type::Slice(..)) { "; read an item of a slice with 'xs[i]'" } else { "" };
                ty.ptr_inner().cloned()
                    .ok_or_else(|| Error::at(span, format!("'^' reads the value a pointer points to, but this value is {}{}", ty, hint)))
            }
            ExprKind::StructInit(name, fields) => {
                if self.sees(name) && self.kinds.contains_key(name.as_str()) {
                    return err(span, format!("'{}' is a kind, not a shape; create its values with its variants", name));
                }
                let shape = self.shapes.get(name.as_str()).filter(|_| self.sees(name))
                    .ok_or_else(|| Error::at(span, format!("unknown shape '{}'{}", name, self.elsewhere(name))))?
                    .clone();
                if module_of(name) != self.current_module
                    && let Some(hidden) = shape.fields.iter().find(|f| f.private)
                {
                    return err(
                        span,
                        format!("'{}' can't be built outside its file, since its field '{}' is private; a task of its module can build it", name, hidden.name),
                    );
                }
                // Fields may be given in any order, each exactly once.
                let mut given: HashMap<String, Span> = HashMap::new();
                let mut declared = Vec::new();
                for field in fields.iter() {
                    let Some(field_def) = shape.fields.iter().find(|f| f.name == field.name) else {
                        return err(field.span, format!("shape '{}' has no field '{}'", name, field.name));
                    };
                    if let Some(at) = given.get(&field.name) {
                        return err(field.span, format!("field '{}' is already given at {}:{}", field.name, at.line, at.col));
                    }
                    given.insert(field.name.clone(), field.span);
                    declared.push(field_def.clone());
                }
                if let Some(missing) = shape.fields.iter().find(|f| !given.contains_key(&f.name)) {
                    return err(span, format!("missing field '{}' in '{}'", missing.name, name));
                }
                let mut values: Vec<&mut Expr> = fields.iter_mut().map(|field| &mut field.value).collect();
                let region = self.check_fields_of_value(&mut values, &declared)?;
                Ok(Type::Named(name.clone(), self.regional.contains(name.as_str()).then_some(region)))
            }
            ExprKind::Range(..) => err(span, "a range can only be used in 'each'"),
            ExprKind::Fail(error) => {
                let ty = self.check_expr(error)?;
                if matches!(ty, Type::Unit | Type::None | Type::Fail(_) | Type::Maybe(_) | Type::Or(..)) {
                    return err(error.span, format!("'fail' needs an error value, got {}", ty));
                }
                Ok(Type::Fail(Box::new(ty)))
            }
            ExprKind::PlanB { value, error, body } => self.check_plan_b(span, value, error.as_ref(), body),
            // Only the checker creates `Wrap`, with its type already set.
            ExprKind::Wrap(_) => expr.ty.clone().ok_or_else(|| Error::internal_at(span, "a wrapped value has no type")),
        }
    }

    /// Checks an array literal, `[a, b, c]`, and returns its type. Kept out of
    /// `infer_expr`, whose stack frame every level of nested code pays for.
    fn check_array(&mut self, values: &mut [Expr], span: Span) -> Result<Type> {
        let mut joined: Option<Type> = None;
        for value in values.iter_mut() {
            let ty = self.check_expr(value)?;
            joined = Some(match joined {
                None => ty,
                Some(before) => join_types(&before, &ty).ok_or_else(|| {
                    Error::at(value.span, format!("an array's values have one type, but this one is {} and the ones before it are {}", ty, before))
                })?,
            });
        }
        let items = joined.unwrap_or(Type::Unit);
        if items == Type::Unit {
            return err(span, "an array can't hold unit, which has no value");
        }
        // `none` alone, or `fail e`, becomes part of a type an annotation gives.
        if !unsettled(&items) {
            for value in values.iter_mut() {
                self.coerce(value, &items)?;
            }
        }
        let ty = Type::Array(Box::new(items), u64::try_from(values.len()).unwrap_or(u64::MAX));
        self.check_size(&ty, span)?;
        Ok(ty)
    }

    /// Checks `[value; count]` outside `new`, an array, and returns its type.
    fn check_repeat(&mut self, value: &mut Expr, count: &mut Expr, span: Span) -> Result<Type> {
        let items = self.check_expr(value)?;
        if items == Type::Unit {
            return err(value.span, "an array can't hold unit, which has no value");
        }
        let count_ty = self.check_expr(count)?;
        self.expect_type(&count_ty, &Type::Int, count.span)?;
        let Some(n) = constant_int(count) else {
            let written = if unsettled(&items) { "T".to_string() } else { items.to_string() };
            return err(
                count.span,
                format!("an array's count is known before the program runs, as in '[0; 8]'; for a count known only as it runs, make a slice in a region, as in 'new [{}]([…; n]) at r'", written),
            );
        };
        let Some(n) = u64::try_from(n).ok().filter(|n| *n > 0) else {
            return err(count.span, "an array holds at least one value, so its count is 1 or more");
        };
        let ty = Type::Array(Box::new(items), n);
        self.check_size(&ty, span)?;
        Ok(ty)
    }

    /// Checks `xs[i]`, an item, or `xs[a..b]`, a slice, on `base` of type
    /// `base_ty`, already checked, and returns its type. A constant index or
    /// bound that is out of range for an array is an error here; the program
    /// checks the rest as it runs.
    fn check_index(&mut self, base: &Expr, base_ty: &Type, index: &mut Expr, span: Span) -> Result<Type> {
        let items = match base_ty {
            Type::Array(items, _) | Type::Slice(items, _) => (**items).clone(),
            Type::Ptr(inner, _) if matches!(**inner, Type::Array(..)) => {
                return err(span, format!("this value is a pointer, {}; index the array it points to, as in 'p^[i]'", base_ty));
            }
            other => {
                return err(span, format!("'[…]' reads an item of an array or slice, but this value is {}{}", other, extract_hint(&[other])));
            }
        };
        let count = match base_ty {
            Type::Array(_, count) => Some(*count),
            _ => None,
        };
        let out_of_range = |value: i64, at: Span, what: &str| -> Result<()> {
            let beyond = u64::try_from(value).map_or(true, |value| count.is_some_and(|count| value > count || (what == "index" && value == count)));
            if !beyond {
                return Ok(());
            }
            match count {
                Some(count) => err(at, format!("{} {} is out of range 0..{} for {}", what, value, count, base_ty)),
                None => err(at, format!("{} {} is out of range: it is less than 0", what, value)),
            }
        };
        let ExprKind::Range(from, to) = &mut index.kind else {
            let ty = self.check_expr(index)?;
            self.expect_type(&ty, &Type::Int, index.span)?;
            if let Some(value) = constant_int(index) {
                out_of_range(value, index.span, "index")?;
            }
            return Ok(items);
        };
        for bound in [&mut **from, &mut **to] {
            let ty = self.check_expr(bound)?;
            self.expect_type(&ty, &Type::Int, bound.span)?;
        }
        let (start, end) = (constant_int(from), constant_int(to));
        for (value, at) in [(start, from.span), (end, to.span)] {
            if let Some(value) = value {
                out_of_range(value, at, "slice bound")?;
            }
        }
        if let (Some(start), Some(end)) = (start, end)
            && end < start
        {
            return err(index.span, format!("slice {}..{} ends before it starts", start, end));
        }
        index.ty = Some(Type::Range(Box::new(Type::Int)));
        let region = match base_ty {
            Type::Slice(_, region) => region.clone(),
            _ => match place_region(base) {
                Some(region) => Some(region),
                None => {
                    return err(
                        span,
                        format!(
                            "a slice points into a region, but this array isn't in one; copy it into a slice, as in 'new [{}](xs) at r', or keep the array in a region with 'new {}(…) at r'",
                            items, base_ty
                        ),
                    );
                }
            },
        };
        Ok(Type::Slice(Box::new(items), region))
    }

    /// Checks the value of `new [T](value) at r`, which gives the slice its
    /// items, of type `items` in `r`: `[v; n]`, whose count may be known only as
    /// the program runs, or an array or slice whose items are copied.
    fn check_slice_source(&mut self, value: &mut Expr, items: &Type, slice: &Type) -> Result<()> {
        if let ExprKind::Repeat { value: item, count } = &mut value.kind {
            self.check_expr(item)?;
            self.coerce(item, items)?;
            let count_ty = self.check_expr(count)?;
            self.expect_type(&count_ty, &Type::Int, count.span)?;
            if let Some(n) = constant_int(count)
                && n < 0
            {
                return err(count.span, format!("count {} is negative", n));
            }
            value.ty = Some(slice.clone());
            return Ok(());
        }
        let ty = self.check_expr(value)?;
        match &ty {
            Type::Array(_, count) => self.coerce(value, &Type::Array(Box::new(items.clone()), *count)),
            Type::Slice(source, _) if fits_regions(source, items) => Ok(()),
            Type::Slice(source, _) => {
                err(value.span, format!("expected type {}, got {}{}", slice, ty, mismatch_hint(source, items)))
            }
            other => err(
                value.span,
                format!("'new [{}](…)' makes a slice from '[value; count]', an array, or a slice, but this value is {}", items, other),
            ),
        }
    }

    /// Checks a plan B, `value { body }`, and returns the type it gives: the
    /// type inside `value`.
    fn check_plan_b(&mut self, span: Span, value: &mut Expr, error: Option<&Binding>, body: &mut Block) -> Result<Type> {
        let value_ty = self.check_expr(value)?;
        let (extracted, error_ty) = match &value_ty {
            Type::Maybe(inner) => ((**inner).clone(), None),
            Type::Or(ok, fail) => ((**ok).clone(), Some((**fail).clone())),
            other => {
                return err(value.span, format!("this value is {}, not a maybe or a 'T or E', so it needs no plan B", other));
            }
        };
        let mut bindings = Vec::new();
        if let Some((name, at)) = error {
            let Some(error_ty) = error_ty else {
                return err(*at, format!("a {} has no error to name; 'fail e' names the error of a 'T or E'", value_ty));
            };
            if self.variant_in_scope(name).is_some() {
                return err(*at, format!("'{}' is a variant; to send an error, write '<- fail {}'", name, name));
            }
            bindings.push((name.clone(), *at, error_ty));
        }
        // The block runs only when there is nothing to extract, so a result it
        // sets isn't set for certain.
        let entry = self.unset.clone();
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _)| name != DISCARD)
            .try_for_each(|(name, at, ty)| self.define(&name, ty, false, at))
            .and_then(|()| self.check_plan_b_body(span, &extracted, body));
        self.pop_scope();
        self.unset = entry;
        let Some((settled, at)) = result? else { return Ok(extracted) };
        // The fallback's regions must outlive the plan B: a region declared in
        // its block is freed when it ends.
        if let Some(region) = settled.regions().into_iter().find(|region| *region != ANY_REGION && self.lookup(region).is_none()) {
            return err(at, format!("this value is in region '{}', which is freed when the plan B ends, so it can't be the plan B's value", region));
        }
        Ok(settled)
    }

    /// Checks a plan B's statements: the last line gives the value, unless every
    /// path through the block sends, or there is no value to give (`unit or E`).
    ///
    /// Returns the plan B's type, and the last line's position, when the last line
    /// settles it: a value holding no pointers (of `ANY_REGION`, such as
    /// `first(none)`) has nothing to extract that lives anywhere, so the plan B
    /// takes the fallback's regions.
    fn check_plan_b_body(&mut self, span: Span, extracted: &Type, body: &mut Block) -> Result<Option<(Type, Span)>> {
        if *extracted == Type::Unit {
            return self.check_block(body).map(|()| None);
        }
        let Some((last, rest)) = body.split_last_mut() else {
            return err(span, format!("a plan B needs a value of type {} on its last line, or a '<-'", extracted));
        };
        self.check_block(rest)?;
        if let StmtKind::Expr(e) = &mut last.kind {
            let ty = self.check_expr(e)?;
            if ty == Type::Unit {
                return err(e.span, format!("the last line of a plan B gives its value, but this has none; expected a value of type {}", extracted));
            }
            if extracted.regions().contains(&ANY_REGION) {
                let mut regions = HashMap::new();
                unify_regions(extracted, &ty, &mut regions);
                let settled = extracted.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| region.to_string())));
                if settled != *extracted {
                    return self.coerce(e, &settled).map(|()| Some((settled, e.span)));
                }
            }
            return self.coerce(e, extracted).map(|()| None);
        }
        self.check_stmt(last)?;
        if !always_sends(body) {
            return err(span, format!("a plan B must end with a value of type {}, or send with '<-' on every path", extracted));
        }
        Ok(None)
    }

    fn check_unary(&mut self, op: UnaryOp, e: &mut Expr, span: Span) -> Result<Type> {
        let ty = self.check_expr(e)?;
        match op {
            UnaryOp::Neg => {
                if !ty.is_numeric() {
                    return err(span, format!("negation requires a number, got {}{}", ty, extract_hint(&[&ty])));
                }
                if let Some(value) = constant_int(e) {
                    fits_int(-i128::from(value), span)?;
                }
                Ok(ty)
            }
            UnaryOp::Not => {
                self.expect_type(&ty, &Type::Bool, e.span)?;
                Ok(Type::Bool)
            }
        }
    }

    fn check_binary(&mut self, op: BinaryOp, l: &mut Expr, r: &mut Expr, span: Span) -> Result<Type> {
        let lt = self.check_expr(l)?;
        let rt = self.check_expr(r)?;
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                if !lt.is_numeric() || !rt.is_numeric() {
                    return err(span, format!("arithmetic requires numeric operands{}", extract_hint(&[&lt, &rt])));
                }
                // Mixing int and float promotes the result to float.
                if lt == Type::Float || rt == Type::Float {
                    return Ok(Type::Float);
                }
                check_constant_arithmetic(op, l, r, span)?;
                Ok(Type::Int)
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let none_and_maybe = matches!((&lt, &rt), (Type::None, Type::Maybe(_)) | (Type::Maybe(_), Type::None));
                if none_and_maybe {
                    return Ok(Type::Bool);
                }
                if lt == Type::None || rt == Type::None {
                    return err(span, "'none' can only be compared with a maybe value");
                }
                if let Some(maybe) = [&lt, &rt].into_iter().find(|t| matches!(t, Type::Maybe(_))) {
                    return err(span, format!("a {} can only be compared with none; extract its value first, as in 'x {{ … }}'", maybe));
                }
                // Comparing addresses stores nothing, so pointers into
                // different regions compare (and are never equal).
                let pointers = matches!((&lt, &rt), (Type::Ptr(..), Type::Ptr(..))) && lt.without_regions() == rt.without_regions();
                if !pointers {
                    self.expect_type(&rt, &lt, r.span)?;
                }
                if !lt.is_numeric() && !matches!(lt, Type::Bool | Type::Text | Type::Ptr(..)) {
                    let symbol = if op == BinaryOp::Eq { "==" } else { "!=" };
                    return err(span, format!("'{}' can't compare values of type {}", symbol, lt));
                }
                Ok(Type::Bool)
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => {
                if !lt.is_numeric() {
                    return err(span, format!("comparison requires numeric operands{}", extract_hint(&[&lt])));
                }
                self.expect_type(&rt, &lt, r.span)?;
                Ok(Type::Bool)
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_type(&lt, &Type::Bool, l.span)?;
                self.expect_type(&rt, &Type::Bool, r.span)?;
                Ok(Type::Bool)
            }
        }
    }

    fn field_type(&self, base: &Type, field: &str, span: Span) -> Result<Type> {
        match base {
            Type::Named(name, region) => {
                let Some(shape) = self.shapes.get(name) else {
                    return err(span, format!("'{}' is a kind and has no fields; use match to read its values", name));
                };
                for f in &shape.fields {
                    if f.name == field {
                        if f.private && module_of(name) != self.current_module {
                            return err(span, format!("'{}' is a private field of '{}', which only its own file can use", field, name));
                        }
                        return Ok(self.field_in(&f.ty, region));
                    }
                }
                err(span, format!("shape '{}' has no field '{}'", name, field))
            }
            Type::Array(..) | Type::Slice(..) if field == "len" => Ok(Type::Int),
            Type::Array(..) | Type::Slice(..) => {
                err(span, format!("type {} has no field '{}'; its number of items is '.len'", base, field))
            }
            Type::Ptr(..) => err(span, format!("type {} has no fields; dereference it first, as in 'p^.{}'", base, field)),
            Type::Maybe(_) | Type::Or(..) => {
                err(span, format!("type {} has no fields; extract its value first, as in 'when x is {{value}}'", base))
            }
            _ => err(span, format!("type {} has no fields", base)),
        }
    }

    /// A field's or payload's declared type `ty`, in a value of region `region`
    /// (none for a value that holds no pointers).
    fn field_in(&self, ty: &Type, region: &Option<String>) -> Type {
        match region {
            Some(region) => self.in_region(ty, region),
            None => ty.clone(),
        }
    }

    /// Checks the values given for a shape's fields or a variant's payload,
    /// `declared` in the same order, and returns the region of the value they
    /// make: the region of the pointers among them, or `ANY_REGION` when they
    /// hold none. Every value must then fit its field in that region.
    fn check_fields_of_value(&mut self, values: &mut [&mut Expr], declared: &[Field]) -> Result<String> {
        const OWN: &str = "";
        let mut regions = HashMap::new();
        for (value, field) in values.iter_mut().zip(declared) {
            let ty = self.check_expr(value)?;
            unify_regions(&self.in_region(&field.ty, OWN), &ty, &mut regions);
        }
        let region = regions.remove(OWN).unwrap_or_else(|| ANY_REGION.to_string());
        for (value, field) in values.iter_mut().zip(declared) {
            self.coerce(value, &self.in_region(&field.ty, &region))?;
        }
        Ok(region)
    }

    /// Checks a call's arguments against the task's `params`, and returns the
    /// type the call gives. A region argument must be a region the caller can
    /// allocate in. The task's other regions stand for the caller's regions
    /// that the arguments' types show; one that no argument shows (as for
    /// `none`) becomes `ANY_REGION`.
    fn check_call_args(&mut self, task: &str, args: &mut [Expr], params: &[(String, Type)], ret: &Type) -> Result<Type> {
        let mut regions: HashMap<String, String> = HashMap::new();
        for (arg, (param, ty)) in args.iter_mut().zip(params) {
            if *ty != Type::Region {
                let actual = self.check_expr(arg)?;
                unify_regions(ty, &actual, &mut regions);
                continue;
            }
            let ExprKind::Var(region) = &arg.kind else {
                return err(arg.span, format!("task '{}' takes a region as '{}', such as a 'region r' declared before the call", task, param));
            };
            self.visible_region(region, arg.span, true)?;
            if let Some(bound) = regions.get(param)
                && bound != region
            {
                return err(arg.span, format!("expected region '{}', got '{}'", bound, region));
            }
            regions.insert(param.clone(), region.clone());
            arg.ty = Some(Type::Region);
        }
        let substitute = |ty: &Type| {
            ty.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())))
        };
        for (arg, (_, ty)) in args.iter_mut().zip(params) {
            if *ty == Type::Region {
                continue;
            }
            let expected = substitute(ty);
            let actual = arg.ty()?.clone();
            let same_but_region = [Some(&expected), expected.extracted()].into_iter().flatten().any(|e| actual.without_regions() == e.without_regions());
            match self.coerce(arg, &expected) {
                Err(_) if same_but_region && !expected.regions().contains(&ANY_REGION) => {
                    return err(
                        arg.span,
                        format!("expected type {}, got {}; task '{}' names one region for this and another argument, so they must be in the same region", expected, actual, task),
                    );
                }
                result => result?,
            }
        }
        Ok(substitute(ret))
    }

    /// Checks that the value of `expr` fits where `expected` is required, and
    /// marks where it becomes part of a `maybe` or `T or E` with a `Wrap` node:
    /// a plain value or `none` in a `maybe`, a plain value or `fail e` in a
    /// `T or E`.
    ///
    /// A value built without pointers (of `ANY_REGION`) fits the same type in
    /// any region, and takes that region; any other value must be of exactly
    /// the expected regions.
    fn coerce(&self, expr: &mut Expr, expected: &Type) -> Result<()> {
        let actual = expr.ty()?.clone();
        if fits_regions(&actual, expected) {
            expr.ty = Some(expected.clone());
            return Ok(());
        }
        if self.coerce_items(expr, expected)? {
            return Ok(());
        }
        match expected {
            Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner) || self.coerce_items(expr, inner)? => {
                wrap(expr, expected);
                return Ok(());
            }
            Type::Or(value, error) => {
                if let Type::Fail(given) = &actual {
                    if !fits_regions(given, error) {
                        return err(expr.span, format!("expected an error of type {}, got {}", error, given));
                    }
                    wrap(expr, expected);
                    return Ok(());
                }
                if **value != Type::Unit {
                    let fits_value = fits_regions(&actual, value)
                        || matches!(value.as_ref(), Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner))
                        || self.coerce_items(expr, value)?;
                    if fits_value {
                        self.coerce(expr, value)?;
                        wrap(expr, expected);
                        return Ok(());
                    }
                }
            }
            _ => {}
        }
        err(expr.span, format!("expected type {}, got {}{}", expected, actual, mismatch_hint(&actual, expected)))
    }

    /// Coerces an array written as a literal item by item, when its values can
    /// become the items of `expected`, as `[1, none]` or `[none; 4]` become
    /// arrays of `maybe int`. Whether `expected` is such an array.
    fn coerce_items(&self, expr: &mut Expr, expected: &Type) -> Result<bool> {
        let Type::Array(items, count) = expected else { return Ok(false) };
        let written = match &expr.ty {
            Some(Type::Array(_, written)) => *written,
            _ => return Ok(false),
        };
        if written != *count {
            return Ok(false);
        }
        match &mut expr.kind {
            ExprKind::Array(values) => values.iter_mut().try_for_each(|value| self.coerce(value, items))?,
            ExprKind::Repeat { value, .. } => self.coerce(value, items)?,
            _ => return Ok(false),
        }
        expr.ty = Some(expected.clone());
        Ok(true)
    }

    /// Checks that a value of type `actual` is exactly `expected`, where no
    /// value changes type: conditions, operands, and ranges.
    fn expect_type(&self, actual: &Type, expected: &Type, span: Span) -> Result<()> {
        if actual != expected {
            err(span, format!("expected type {}, got {}{}", expected, actual, extract_hint(&[actual])))
        } else {
            Ok(())
        }
    }
}

/// Replaces `expr` with a `Wrap` of itself, of type `ty`.
fn wrap(expr: &mut Expr, ty: &Type) {
    let span = expr.span;
    let inner = std::mem::replace(expr, Expr::new(ExprKind::NoneLit, span));
    *expr = Expr { kind: ExprKind::Wrap(Box::new(inner)), span, ty: Some(ty.clone()) };
}

/// A hint for an operation on a `maybe` or `T or E`, whose value must be
/// extracted first; empty if none of `types` is one.
fn extract_hint(types: &[&Type]) -> String {
    match types.iter().find(|t| matches!(t, Type::Maybe(_) | Type::Or(..) | Type::Fail(_))) {
        Some(Type::Fail(_)) => "; 'fail' takes only the value right after it, so write 'fail (a + b)'".to_string(),
        Some(Type::Or(value, error)) if **value == Type::Unit => {
            format!("; a 'unit or {}' has no value, only an error to handle, as in 'x {{ fail e … }}'", error)
        }
        Some(ty) => format!("; the value in this {} must be extracted first, as in 'x {{ … }}' or 'when x is {{value}}'", ty),
        None => String::new(),
    }
}

/// The shapes and kinds that hold pointers, directly or in other shapes and
/// kinds by value, so their values live in a region.
pub fn regional_types(items: &[Item]) -> HashSet<String> {
    let fields_of = |item: &Item| -> Vec<Type> {
        match item {
            Item::Shape(s) => s.fields.iter().map(|f| f.ty.clone()).collect(),
            Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).map(|f| f.ty.clone()).collect(),
            Item::Task(_) => Vec::new(),
        }
    };
    // The items whose fields name each type, to look at again when it turns
    // out to be regional.
    let mut users: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        for ty in fields_of(item) {
            for name in named_types(&ty) {
                users.entry(name.to_string()).or_default().push(i);
            }
        }
    }
    let mut regional = HashSet::new();
    let mut pending: Vec<usize> = (0..items.len()).collect();
    while let Some(i) = pending.pop() {
        let name = match items.get(i) {
            Some(Item::Shape(s)) => &s.name,
            Some(Item::Kind(k)) => &k.name,
            _ => continue,
        };
        if !regional.contains(name) && items.get(i).map(fields_of).unwrap_or_default().iter().any(|ty| holds_pointers(ty, &regional)) {
            regional.insert(name.clone());
            pending.extend(users.get(name).into_iter().flatten().copied());
        }
    }
    regional
}

/// A field's type, or the type after `new`, whose regions are unsettled, as it
/// is in a value of region `region`: its pointers and its shapes and kinds in
/// `regional` get `region`.
pub fn in_region(ty: &Type, region: &str, regional: &HashSet<String>) -> Type {
    ty.map_regions_of_names(&mut |name| (name.is_empty() || regional.contains(name)).then(|| region.to_string()))
}

/// Whether a value of type `actual` fits where `expected` is required without
/// changing: the same type, where `actual` may be of `ANY_REGION` wherever
/// `expected` names a region.
fn fits_regions(actual: &Type, expected: &Type) -> bool {
    let region = |a: &Option<String>, e: &Option<String>| a == e || a.as_deref() == Some(ANY_REGION);
    match (actual, expected) {
        (Type::Ptr(a, ar), Type::Ptr(e, er)) | (Type::Slice(a, ar), Type::Slice(e, er)) => region(ar, er) && fits_regions(a, e),
        (Type::Array(a, an), Type::Array(e, en)) => an == en && fits_regions(a, e),
        (Type::Named(a, ar), Type::Named(e, er)) => a == e && region(ar, er),
        (Type::Range(a), Type::Range(e)) | (Type::Maybe(a), Type::Maybe(e)) | (Type::Fail(a), Type::Fail(e)) => fits_regions(a, e),
        (Type::Or(av, ae), Type::Or(ev, ee)) => fits_regions(av, ev) && fits_regions(ae, ee),
        _ => actual == expected,
    }
}

/// Records in `regions` what each region of `pattern` stands for in `actual`,
/// following `actual` into the places a value can become part of `pattern`
/// (`maybe`, `T or E`, `fail`). A region already recorded, or one `actual`
/// doesn't show, is left as it is; checking the value against the settled type
/// then reports any mismatch.
pub fn unify_regions(pattern: &Type, actual: &Type, regions: &mut HashMap<String, String>) {
    let mut record = |p: &Option<String>, a: &Option<String>| {
        if let (Some(p), Some(a)) = (p, a)
            && a != ANY_REGION
        {
            regions.entry(p.clone()).or_insert_with(|| a.clone());
        }
    };
    match (pattern, actual) {
        (Type::Ptr(p, pr), Type::Ptr(a, ar)) | (Type::Slice(p, pr), Type::Slice(a, ar)) => {
            record(pr, ar);
            unify_regions(p, a, regions);
        }
        (Type::Array(p, _), Type::Array(a, _)) => unify_regions(p, a, regions),
        (Type::Named(p, pr), Type::Named(a, ar)) if p == a => record(pr, ar),
        (Type::Maybe(p), Type::Maybe(a)) | (Type::Range(p), Type::Range(a)) | (Type::Fail(p), Type::Fail(a)) => unify_regions(p, a, regions),
        (Type::Or(pv, pe), Type::Or(av, ae)) => {
            unify_regions(pv, av, regions);
            unify_regions(pe, ae, regions);
        }
        (Type::Or(_, pe), Type::Fail(a)) => unify_regions(pe, a, regions),
        (Type::Or(pv, _), a) => unify_regions(pv, a, regions),
        (Type::Maybe(p), a) => unify_regions(p, a, regions),
        _ => {}
    }
}

/// Whether a value of type `ty` holds a pointer, given the shapes and kinds
/// in `regional` that do.
fn holds_pointers(ty: &Type, regional: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) | Type::Slice(..) => true,
        Type::Named(name, _) => regional.contains(name),
        Type::Maybe(inner) | Type::Array(inner, _) => holds_pointers(inner, regional),
        Type::Or(value, error) => holds_pointers(value, regional) || holds_pointers(error, regional),
        _ => false,
    }
}

/// Whether `ty` still holds `none` or `fail …` where a type belongs, as the
/// array `[none, none]` does until a type written for it settles what it holds.
fn unsettled(ty: &Type) -> bool {
    match ty {
        Type::None | Type::Fail(_) => true,
        Type::Array(items, _) => unsettled(items),
        _ => false,
    }
}

/// `ty` with a type in place of each `none` and `fail e`, for a message
/// suggesting an annotation: `[maybe int; 2]` for `[none; 2]`.
fn settled_example(ty: &Type) -> Type {
    match ty {
        Type::None => Type::Maybe(Box::new(Type::Int)),
        Type::Fail(error) => Type::Or(Box::new(Type::Int), error.clone()),
        Type::Array(items, count) => Type::Array(Box::new(settled_example(items)), *count),
        other => other.clone(),
    }
}

/// The type the values of types `a` and `b` can both become, for the items of
/// an array: either, when the other fits it; a `maybe` for `none` and a value;
/// a `T or E` for `fail e` and a value. `None` when there is none.
fn join_types(a: &Type, b: &Type) -> Option<Type> {
    if fits_regions(b, a) {
        return Some(a.clone());
    }
    if fits_regions(a, b) {
        return Some(b.clone());
    }
    let plain = |ty: &Type| !matches!(ty, Type::None | Type::Fail(_) | Type::Unit | Type::Maybe(_) | Type::Or(..));
    match (a, b) {
        (Type::Array(x, n), Type::Array(y, m)) if n == m => join_types(x, y).map(|items| Type::Array(Box::new(items), *n)),
        (Type::None, Type::Maybe(_)) => Some(b.clone()),
        (Type::Maybe(_), Type::None) => Some(a.clone()),
        (Type::None, other) | (other, Type::None) if plain(other) => Some(Type::Maybe(Box::new(other.clone()))),
        (Type::Maybe(inner), other) | (other, Type::Maybe(inner)) if plain(other) => {
            join_types(inner, other).filter(plain).map(|value| Type::Maybe(Box::new(value)))
        }
        (Type::Fail(error), Type::Or(_, known)) | (Type::Or(_, known), Type::Fail(error)) if fits_regions(error, known) => {
            Some(if matches!(a, Type::Or(..)) { a.clone() } else { b.clone() })
        }
        (Type::Fail(error), other) | (other, Type::Fail(error)) if plain(other) || matches!(other, Type::Maybe(_)) => {
            (!fits_regions(other, error) && !fits_regions(error, other)).then(|| Type::Or(Box::new(other.clone()), error.clone()))
        }
        (Type::Or(value, error), other) | (other, Type::Or(value, error)) if !matches!(other, Type::Fail(_)) => {
            join_types(value, other).filter(|joined| !matches!(joined, Type::Or(..))).map(|joined| Type::Or(Box::new(joined), error.clone()))
        }
        _ => None,
    }
}

/// The region of the array `place` names, when it is in one: behind a
/// pointer, or an item of a slice. `None` for an array in a variable or a
/// temporary value.
fn place_region(place: &Expr) -> Option<String> {
    match &place.kind {
        ExprKind::Deref(pointer) => match pointer.ty.as_ref()? {
            Type::Ptr(_, region) => region.clone(),
            _ => None,
        },
        ExprKind::Field(base, _) => place_region(base),
        ExprKind::Index(base, _) => match base.ty.as_ref()? {
            Type::Slice(_, region) => region.clone(),
            _ => place_region(base),
        },
        _ => None,
    }
}

/// At most how many bytes a value of type `ty` takes in C, given `sizes` for
/// shapes and kinds: each part counts at least 8 bytes, which covers padding.
fn value_bytes(ty: &Type, sizes: &HashMap<String, u128>) -> u128 {
    match ty {
        Type::Unit => 0,
        Type::Named(name, _) => sizes.get(name).copied().unwrap_or(8),
        Type::Array(items, count) => value_bytes(items, sizes).saturating_mul(u128::from(*count)),
        Type::Slice(..) => 16,
        Type::Maybe(inner) => value_bytes(inner, sizes).saturating_add(8),
        Type::Or(value, error) => value_bytes(value, sizes).saturating_add(value_bytes(error, sizes)).saturating_add(8),
        Type::Fail(inner) | Type::Range(inner) => value_bytes(inner, sizes),
        Type::Results(types) => types.iter().map(|ty| value_bytes(ty, sizes).max(8)).fold(0, u128::saturating_add),
        _ => 8,
    }
}

/// At most how many bytes a shape's fields or a variant's payload take.
fn fields_bytes(fields: &[Field], sizes: &HashMap<String, u128>) -> u128 {
    fields.iter().map(|field| value_bytes(&field.ty, sizes).max(8)).fold(8, u128::saturating_add)
}

/// Whether a task with return type `ret` must send a value: every return type
/// except `unit` and `unit or E`, which succeed by reaching their end.
fn sends_value(ret: &Type) -> bool {
    !matches!(ret, Type::Unit) && !matches!(ret, Type::Or(value, _) if **value == Type::Unit)
}

/// The shapes and kinds a value of type `ty` contains by value (not behind a
/// pointer).
fn contained_by_value(ty: &Type) -> Vec<&str> {
    match ty {
        Type::Named(name, _) => vec![name.as_str()],
        Type::Maybe(inner) | Type::Array(inner, _) => contained_by_value(inner),
        Type::Or(value, error) => {
            let mut names = contained_by_value(value);
            names.extend(contained_by_value(error));
            names
        }
        _ => Vec::new(),
    }
}

/// Every shape or kind a type names, by value or through pointers.
fn named_types(ty: &Type) -> Vec<&str> {
    match ty {
        Type::Named(name, _) => vec![name.as_str()],
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Fail(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => named_types(inner),
        Type::Or(value, error) => {
            let mut names = named_types(value);
            names.extend(named_types(error));
            names
        }
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` can be created, given the shapes and kinds in
/// `can` that can be.
fn constructible(ty: &Type, can: &HashSet<&str>) -> bool {
    match ty {
        Type::Named(name, _) => can.contains(name.as_str()),
        Type::Ptr(inner, _) | Type::Array(inner, _) | Type::Slice(inner, _) => constructible(inner, can),
        Type::Or(value, error) => **value == Type::Unit || constructible(value, can) || constructible(error, can),
        _ => true,
    }
}

/// Whether running `block` always ends in `<-`: it contains a `<-`, a
/// `when` whose two branches both always send, or a `match` whose cases all do.
/// Loops don't count, since their bodies might not run.
fn always_sends(block: &Block) -> bool {
    block.iter().any(|stmt| match &stmt.kind {
        StmtKind::Send(_) => true,
        StmtKind::When(_, then_block, Some(else_block)) => always_sends(then_block) && always_sends(else_block),
        StmtKind::Match(_, cases) => cases.iter().all(|case| always_sends(&case.body)),
        _ => false,
    })
}

/// The value of an int expression written with literals only (`-5`,
/// `60 * 60 * 24`), which the program computes the same way every time, or
/// `None` for any other expression. Its parts have been checked, so each of
/// them fits in an `int`.
fn constant_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Unary(UnaryOp::Neg, operand) => constant_int(operand)?.checked_neg(),
        ExprKind::Binary(op, l, r) => {
            let (a, b) = (constant_int(l)?, constant_int(r)?);
            match op {
                BinaryOp::Add => a.checked_add(b),
                BinaryOp::Sub => a.checked_sub(b),
                BinaryOp::Mul => a.checked_mul(b),
                BinaryOp::Div => a.checked_div(b),
                // The remainder of a division by -1 is 0, which Rust's
                // `checked_rem` leaves out for the smallest int.
                BinaryOp::Mod if b == -1 => Some(0),
                BinaryOp::Mod => a.checked_rem(b),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Rejects int arithmetic the program would stop on every time it runs: a
/// division or remainder by a constant zero, and a constant whose result
/// doesn't fit in an `int`. Anything else is checked as the program runs.
fn check_constant_arithmetic(op: BinaryOp, l: &Expr, r: &Expr, span: Span) -> Result<()> {
    let divisor = constant_int(r);
    if divisor == Some(0) {
        match op {
            BinaryOp::Div => return err(span, "division by zero"),
            BinaryOp::Mod => return err(span, "remainder of division by zero"),
            _ => {}
        }
    }
    let (Some(a), Some(b)) = (constant_int(l), divisor) else { return Ok(()) };
    let (a, b) = (i128::from(a), i128::from(b));
    let exact = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        _ => return Ok(()),
    };
    fits_int(exact, span)
}

/// Rejects `value`, the exact result of a constant int operation at `span`,
/// when it doesn't fit in an `int`.
fn fits_int(value: i128, span: Span) -> Result<()> {
    if value > i128::from(i64::MAX) {
        return err(span, format!("integer overflow: the result, {}, is more than an int holds (at most {})", value, i64::MAX));
    }
    if value < i128::from(i64::MIN) {
        return err(span, format!("integer overflow: the result, {}, is less than an int holds (at least {})", value, i64::MIN));
    }
    Ok(())
}

/// Why a value of type `actual` doesn't fit where `expected` is required, or
/// nothing when there is nothing useful to say.
fn mismatch_hint(actual: &Type, expected: &Type) -> String {
    match (actual, expected) {
        (Type::None, Type::Ptr(..)) => format!("; a pointer can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Slice(..)) => format!("; a slice can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Or(value, _)) if value.ptr_inner().is_some() => {
            format!("; a pointer can't be none, so use 'maybe {}'", value)
        }
        (_, Type::Or(_, error)) if fits_regions(actual, error) => "; an error is given with 'fail', as in '<- fail e'".to_string(),
        (Type::Fail(_), _) => "; 'fail …' is an error, which only a 'T or E' can hold".to_string(),
        (Type::Maybe(inner) | Type::Or(inner, _), _) if fits_regions(inner, expected) => {
            "; extract its value first, as in 'x { … }' or 'when x is {value}'".to_string()
        }
        _ if [Some(expected), expected.extracted()].into_iter().flatten().any(|e| actual.without_regions() == e.without_regions()) => {
            if expected.regions().contains(&ANY_REGION) {
                "; this place holds a value without pointers (of no region), so it can't take a pointer into one".to_string()
            } else {
                "; a value can't move to another region: a pointer into one region is never stored in another".to_string()
            }
        }
        _ => String::new(),
    }
}

/// Checks that a value of type `actual`, one of the several a call sends, fits
/// where `expected` is required. Nothing can change it on the way, so unlike a
/// single value it never becomes a `maybe` or a `T or E` here.
fn fits_or_err(actual: &Type, expected: &Type, at: Span) -> Result<()> {
    if fits_regions(actual, expected) {
        return Ok(());
    }
    let wrapping = match expected {
        Type::Maybe(inner) => *actual == Type::None || fits_regions(actual, inner),
        Type::Or(value, error) => match actual {
            Type::Fail(given) => fits_regions(given, error),
            _ => fits_regions(actual, value),
        },
        _ => false,
    };
    let hint = if wrapping {
        format!("; a task sends each of its values as it is, so it doesn't become a {} on the way", expected)
    } else {
        mismatch_hint(actual, expected)
    };
    err(at, format!("expected type {}, got {}{}", expected, actual, hint))
}

/// The error for a call whose `T or E` result is thrown away.
fn ignored_error(task: &str) -> String {
    format!("the error '{0}' may send is ignored; handle it with a plan B, as in '{0}(…) {{ fail e … }}', or with match", task)
}

/// The error for a `_` that would drop a value holding an error.
fn dropped_error(ty: &Type) -> String {
    format!("'_' would drop the error in a {}; give it a name, then handle it with a plan B or match", ty)
}

/// Example names for a list of `n` values, as in `bind a, b = …`.
fn example_names(n: usize) -> String {
    let names: Vec<String> = (0..n.min(3)).filter_map(|i| u8::try_from(i).ok()).map(|i| char::from(b'a' + i).to_string()).collect();
    if n > 3 { format!("{}, …", names.join(", ")) } else { names.join(", ") }
}

/// `1 argument`, `2 arguments`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {}", noun) } else { format!("{} {}s", n, noun) }
}

/// The program's name for `name` declared in module `module`: `module.name`,
/// or just `name` in the main file.
fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() { name.to_string() } else { format!("{}{}{}", module, MODULE_SEPARATOR, name) }
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
