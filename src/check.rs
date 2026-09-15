use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Error, Result};

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
    /// Every variant, by tag. Tags are unique across all kinds.
    variants: HashMap<String, VariantInfo>,
    scopes: Vec<HashMap<String, VarInfo>>,
    current_ret: Option<Type>,
}

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check_file(&mut self, items: &mut [Item]) -> Result<()> {
        self.collect_declarations(items)?;
        self.regional = regional_types(items);
        self.check_declarations(items)?;
        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.check_task(t)?;
            }
        }
        Ok(())
    }

    /// Registers every task, shape, kind, and variant. They share one
    /// namespace: no two may have the same name, and none may use the name of
    /// a builtin type or task.
    fn collect_declarations(&mut self, items: &[Item]) -> Result<()> {
        let mut names: HashMap<String, Span> = HashMap::new();
        let mut declare = |name: &str, span: Span| -> Result<()> {
            // No declaration may take the name of a builtin type or task.
            if Type::builtin(name).is_some() {
                return err(span, format!("'{}' is a builtin type and cannot be redefined", name));
            }
            if BUILTIN_TASKS.contains(&name) {
                return err(span, format!("'{}' is a builtin task and cannot be redefined", name));
            }
            if let Some(at) = names.get(name) {
                return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
            }
            names.insert(name.to_string(), span);
            Ok(())
        };
        for item in items {
            match item {
                Item::Shape(s) => {
                    declare(&s.name, s.span)?;
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    declare(&k.name, k.span)?;
                    for v in &k.variants {
                        declare(&v.tag, v.span)?;
                        let info = VariantInfo { kind: k.name.clone(), variant: v.clone() };
                        self.variants.insert(v.tag.clone(), info);
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
                            return err(t.ret_span, "task 'main' cannot return a value");
                        }
                    }
                }
            }
        }
        let mut acyclic = HashSet::new();
        for item in items.iter() {
            let name = match item {
                Item::Shape(s) => &s.name,
                Item::Kind(k) => &k.name,
                Item::Task(_) => continue,
            };
            self.check_value_cycle(name, &mut acyclic)?;
        }
        self.check_constructible(items)
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

    /// Checks a task's parameters and return type, and settles their regions.
    /// A region the header names without a `region` parameter is one the task
    /// only reads or links pointers in (it can't allocate there), and the
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
                    return err(param.ty_span, format!("'{}' is a parameter, not a region", other.name));
                }
                if !named.iter().any(|n| n == region) {
                    named.push(region.to_string());
                }
            }
            resolved.push(ty);
        }
        if t.ret == Type::Region {
            return err(t.ret_span, "a task can't send a region; regions are only parameters, as in 'task build(r: region)'");
        }
        if t.ret != Type::Unit {
            let ret = self.resolve_type(&t.ret, t.ret_span, TypePlace::Header)?;
            if let Some(region) = ret.regions().into_iter().find(|r| !named.iter().any(|n| n == r)) {
                return err(
                    t.ret_span,
                    format!(
                        "the result lives in region '{0}', which the task isn't given; take it as a parameter, as in '{0}: region'",
                        region
                    ),
                );
            }
            t.ret = ret;
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
        self.resolve_regions(ty, span, place, None)
    }

    /// `resolve_type` for `ty` inside a pointer of region `inside`.
    fn resolve_regions(&self, ty: &Type, span: Span, place: TypePlace, inside: Option<&str>) -> Result<Type> {
        let given = |region: &Option<String>| -> Result<()> {
            match (place, region) {
                (TypePlace::Field, Some(region)) => err(
                    span,
                    format!("a field lives in the region of the value holding it, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::New, Some(region)) => err(
                    span,
                    format!("the value 'new' makes lives in the region it allocates in, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::Body, Some(region)) => self.visible_region(region, span, false),
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
            Type::Named(name, region) => {
                given(region)?;
                if !self.regional.contains(name) {
                    if let Some(region) = region
                        && inside != Some(region.as_str())
                    {
                        return err(span, format!("'{}' holds no pointers, so it lives in no region; remove '@{}'", name, region));
                    }
                    return Ok(Type::named(name));
                }
                if needs_region && region.is_none() {
                    return err(span, format!("'{0}' holds pointers, so it lives in a region; name it, as in '{0}@r'", name));
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
            Type::Unit => err(span, "unit is only allowed as a return type"),
            Type::Region => err(span, "'region' is only the type of a task's parameter, as in 'task build(r: region)'"),
            Type::Ptr(inner, _) => self.check_type(inner, span),
            Type::Maybe(inner) => match inner.as_ref() {
                Type::Maybe(_) => err(span, "a maybe can't hold another maybe"),
                Type::Unit => err(span, "a maybe can't hold unit, which has no value"),
                _ => self.check_type(inner, span),
            },
            Type::Or(value, error) => {
                if **value != Type::Unit {
                    self.check_type(value, span)?;
                }
                match error.as_ref() {
                    Type::Unit => return err(span, "the error type can't be unit"),
                    Type::Maybe(_) => return err(span, "the error type can't be a maybe"),
                    _ => self.check_type(error, span)?,
                }
                if value == error {
                    return err(span, format!("the error type must differ from the value type, as in '{} or ParseError'", value));
                }
                Ok(())
            }
            Type::Named(name, _) if !self.shapes.contains_key(name) && !self.kinds.contains_key(name) => {
                err(span, format!("unknown type '{}'", name))
            }
            _ => Ok(()),
        }
    }

    /// Rejects a shape or kind that contains itself by value, directly or
    /// through other types: its values would have infinite size. Containing a
    /// pointer to itself (`maybe ^T`) is fine. `acyclic` holds the types
    /// already known to be free of cycles. The search keeps its own stack, so
    /// a long chain of types can't overflow the compiler's.
    fn check_value_cycle(&self, root: &str, acyclic: &mut HashSet<String>) -> Result<()> {
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
        self.check_block(&mut task.body)?;
        self.scopes.clear();
        self.current_ret = None;
        if sends_value(&task.ret) && !always_sends(&task.body) {
            return err(task.span, format!("task '{}' can finish without sending a value", task.name));
        }
        Ok(())
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
        if let Some(info) = self.variants.get(name) {
            let at = info.variant.span;
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        if let Some(existing) = self.lookup(name) {
            let at = existing.span;
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        let Some(scope) = self.scopes.last_mut() else {
            return Err(Error::internal(format!("'{}' defined outside of a task", name)));
        };
        let allocates = ty == Type::Region;
        scope.insert(name.to_string(), VarInfo { ty, mutable, span, allocates });
        Ok(())
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
            return err(span, format!("'{0}' is a region, not a value; allocate in it with 'new T(…) in {0}'", name));
        }
        self.lookup(name).ok_or_else(|| {
            if self.variants.contains_key(name) {
                Error::at(span, format!("'{}' is a variant, not a variable", name))
            } else if let Some(shape) = self.shapes.get(name) {
                // Most likely a shape literal the parser read as a plan B.
                let example = match shape.fields.first() {
                    Some(field) => format!("{} {{ {}: … }}", name, field.name),
                    None => format!("{} {{}}", name),
                };
                Error::at(span, format!("'{}' is a shape, not a variable; a literal names its fields on one line, as in '{}'", name, example))
            } else {
                Error::at(span, format!("unknown variable '{}'", name))
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
                self.check_expr(e)?;
                // Of all expressions, only a call can have an effect, and so
                // can a plan B on a call, or one that gives no value.
                match &e.kind {
                    ExprKind::Call(name, _) if matches!(e.ty, Some(Type::Or(..))) => err(
                        e.span,
                        format!("the error '{}' may send is ignored; handle it with a plan B, as in '{}(…) {{ fail e … }}', or with match", name, name),
                    ),
                    ExprKind::Call(..) => Ok(()),
                    ExprKind::PlanB { value, .. } if e.ty == Some(Type::Unit) || matches!(value.kind, ExprKind::Call(..)) => Ok(()),
                    ExprKind::Binary(BinaryOp::Eq, ..) => err(e.span, "the result of '==' is unused; use '=' to assign"),
                    _ => err(e.span, "this value is unused; only a task call can be used as a statement"),
                }
            }
            StmtKind::Binding { name, mutable, ty, ty_span, init } => {
                if let Some(annotated) = ty {
                    *annotated = self.resolve_type(annotated, ty_span.unwrap_or(span), TypePlace::Body)?;
                }
                let init_ty = self.check_expr(init)?;
                let binding_ty = match ty {
                    Some(annotated) => {
                        self.coerce(init, annotated)?;
                        annotated.clone()
                    }
                    None => match init_ty {
                        Type::None => {
                            return err(init.span, format!("cannot infer the type of '{}' from none; give it a type, such as '{}: maybe int'", name, name));
                        }
                        Type::Fail(error) => {
                            return err(init.span, format!("cannot infer the type of '{}' from 'fail …'; give it a type, such as '{}: int or {}'", name, name, error));
                        }
                        Type::Unit => return err(init.span, "this expression has no value (its type is unit)"),
                        other if other.regions().contains(&ANY_REGION) => {
                            let example = other.map_regions(&mut |region| Some(if region == ANY_REGION { "r" } else { region }.to_string()));
                            return err(
                                init.span,
                                format!(
                                    "cannot tell which region '{0}' lives in, since its value holds no pointers yet; give it a type, such as '{0}: {1}'",
                                    name, example
                                ),
                            );
                        }
                        other => other,
                    },
                };
                *ty = Some(binding_ty.clone());
                self.define(name, binding_ty, *mutable, span)
            }
            StmtKind::Assign(lhs, rhs) => {
                let lhs_ty = self.check_assign_target(lhs)?;
                self.check_expr(rhs)?;
                self.coerce(rhs, &lhs_ty)
            }
            StmtKind::When(cond, then_block, else_block) => {
                self.check_condition(cond, then_block)?;
                if let Some(else_b) = else_block {
                    self.check_scoped_block(else_b)?;
                }
                Ok(())
            }
            StmtKind::Loop(cond, body) => self.check_condition(cond, body),
            StmtKind::Each(name, range, body) => {
                // A range is only valid here, so it is checked here rather than
                // as an ordinary expression.
                let ExprKind::Range(start, end) = &mut range.kind else {
                    return err(range.span, "each requires a range, such as 0..n");
                };
                for bound in [start, end] {
                    let bty = self.check_expr(bound)?;
                    self.expect_type(&bty, &Type::Int, bound.span)?;
                }
                range.ty = Some(Type::Range(Box::new(Type::Int)));
                // The loop variable and the body share one scope.
                self.push_scope();
                let result = self.define(name, Type::Int, false, span)
                    .and_then(|()| self.check_block(body));
                self.pop_scope();
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases),
            StmtKind::Send(expr) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                match expr {
                    Some(e) if ret == Type::Unit => err(e.span, "this task has no return type, so '<-' cannot take a value"),
                    Some(e) => {
                        let ety = self.check_expr(e)?;
                        if !sends_value(&ret) && !matches!(ety, Type::Fail(_)) && ety != ret {
                            return err(e.span, "this task sends no value when it succeeds; write a bare '<-', or '<- fail …' to send an error");
                        }
                        self.coerce(e, &ret)
                    }
                    None if sends_value(&ret) => err(span, "expected a value after '<-'"),
                    None => Ok(()),
                }
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
            && self.variants.contains_key(name)
        {
            let (keyword, whole) = if matches!(pattern.kind, PatternKind::Fail(_)) { ("fail e", "error") } else { ("{value}", "value") };
            return err(*at, format!("'{}' is a variant, but '{}' names the whole {}; give it a name, then match on it", name, keyword, whole));
        }
        match (&pattern.kind, subject) {
            (PatternKind::Any | PatternKind::None, Type::Maybe(_)) => Ok(Vec::new()),
            (PatternKind::Any, _) => Ok(Vec::new()),
            (PatternKind::Variant { tag, binds }, Type::Named(kind_name, region)) if self.kinds.contains_key(kind_name) => {
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
                if self.variants.contains_key(tag) {
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
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Deref(_) => self.check_expr(expr),
            _ => err(span, "invalid assignment target"),
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
            ExprKind::Field(..) | ExprKind::Deref(_) => self.check_assign_target(base),
            _ => err(span, "cannot assign to a field of a temporary value"),
        }
    }

    fn check_match(&mut self, span: Span, expr: &mut Expr, cases: &mut [Case]) -> Result<()> {
        let ety = self.check_expr(expr)?;
        // The cases that together cover every value.
        let needed: Vec<String> = match &ety {
            Type::Named(name, _) => match self.kinds.get(name) {
                Some(kind) => kind.variants.iter().map(|v| v.tag.clone()).collect(),
                None => return err(expr.span, format!("'{}' is not a kind", name)),
            },
            Type::Maybe(_) => vec!["{…}".to_string(), "none".to_string()],
            Type::Or(..) => vec!["{…}".to_string(), "fail …".to_string()],
            other => return err(expr.span, format!("match requires a kind, a maybe, or a 'T or E' value, got {}", other)),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut matched_all = false;
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
        }
        if !matched_all
            && let Some(missing) = needed.iter().find(|n| !seen.contains(n))
        {
            return err(span, format!("missing case '{}' in match", missing));
        }
        Ok(())
    }

    /// Checks an expression, records its type on the node, and returns it.
    fn check_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
        self.resolve_empty_plan_b(expr);
        let ty = self.infer_expr(expr)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    /// Rewrites a variant used as a value, `Empty`, or called, `Circle(1.0)`,
    /// into a `Construct` node. Variables can't share a variant's name, so a
    /// bare name is never ambiguous.
    fn resolve_constructor(&self, expr: &mut Expr) {
        let variant_kind = |tag: &str| self.variants.get(tag).map(|info| info.kind.clone());
        let resolved = match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => {
                variant_kind(name).map(|kind| (kind, std::mem::take(name), Vec::new()))
            }
            ExprKind::Call(name, args) => {
                variant_kind(name).map(|kind| (kind, std::mem::take(name), std::mem::take(args)))
            }
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
        if !fields.is_empty() || self.shapes.contains_key(name.as_str()) || self.lookup(name).is_none() {
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
            ExprKind::Var(name) => Ok(self.lookup_or_err(name, span)?.ty.clone()),
            ExprKind::Field(base, field) => {
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field, span)
            }
            ExprKind::Index(_base, _index) => {
                // Arrays not supported yet
                err(span, "indexing not supported")
            }
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
                        return err(arg.span, "show expects int, float, bool, or text");
                    }
                    return Ok(Type::Unit);
                }
                let (params, ret) = self.tasks.get(name.as_str())
                    .ok_or_else(|| {
                        // `send (x)` written the old way reads as a call.
                        let hint = match name.as_str() {
                            "send" => "; a task sends its result with '<-'",
                            "free" => "; Vela has no 'free': memory in a region is freed when the block declaring the region closes",
                            _ => "",
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
                let variant = self.variants.get(tag.as_str())
                    .map(|info| info.variant.clone())
                    .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
                if args.len() != variant.payload.len() {
                    return err(span, format!("variant '{}' expects {}, got {}", tag, count(variant.payload.len(), "argument"), args.len()));
                }
                let mut values: Vec<&mut Expr> = args.iter_mut().collect();
                let region = self.check_fields_of_value(&mut values, &variant.payload)?;
                Ok(Type::Named(kind.clone(), self.regional.contains(kind.as_str()).then_some(region)))
            }
            ExprKind::New { ty, ty_span, value, region, region_span } => {
                *ty = self.resolve_type(ty, *ty_span, TypePlace::New)?;
                // `^int or E` means `(^int) or E`, so no written type could hold the pointer.
                if matches!(ty, Type::Or(..)) {
                    return err(*ty_span, format!("'new' can't make a pointer to '{}'; extract the value first, or keep it in a shape's field", ty));
                }
                self.visible_region(region, *region_span, true)?;
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
                ty.ptr_inner().cloned()
                    .ok_or_else(|| Error::at(span, "cannot dereference non-pointer"))
            }
            ExprKind::StructInit(name, fields) => {
                if self.kinds.contains_key(name.as_str()) {
                    return err(span, format!("'{}' is a kind, not a shape; create its values with its variants", name));
                }
                let shape = self.shapes.get(name.as_str())
                    .ok_or_else(|| Error::at(span, format!("unknown shape '{}'", name)))?
                    .clone();
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
            if self.variants.contains_key(name) {
                return err(*at, format!("'{}' is a variant; to send an error, write '<- fail {}'", name, name));
            }
            bindings.push((name.clone(), *at, error_ty));
        }
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _)| name != DISCARD)
            .try_for_each(|(name, at, ty)| self.define(&name, ty, false, at))
            .and_then(|()| self.check_plan_b_body(span, &extracted, body));
        self.pop_scope();
        Ok(result?.unwrap_or(extracted))
    }

    /// Checks a plan B's statements: the last line gives the value, unless every
    /// path through the block sends, or there is no value to give (`unit or E`).
    ///
    /// Returns the plan B's type when the last line settles it: a value holding
    /// no pointers (of `ANY_REGION`, such as `first(none)`) has nothing to
    /// extract that lives anywhere, so the plan B takes the fallback's regions.
    fn check_plan_b_body(&mut self, span: Span, extracted: &Type, body: &mut Block) -> Result<Option<Type>> {
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
            if !fits_regions(&ty, extracted) && fits_regions(extracted, &ty) {
                return Ok(Some(ty));
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
                    return err(span, format!("negation requires numeric type{}", extract_hint(&[&ty])));
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
                    Ok(Type::Float)
                } else {
                    Ok(Type::Int)
                }
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
                    return err(span, "cannot compare this type");
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
                        return Ok(self.field_in(&f.ty, region));
                    }
                }
                err(span, format!("shape '{}' has no field '{}'", name, field))
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
            if *ty != Type::Region {
                self.coerce(arg, &substitute(ty))?;
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
        match expected {
            Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner) => {
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
                        || matches!(value.as_ref(), Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner));
                    if fits_value {
                        self.coerce(expr, value)?;
                        wrap(expr, expected);
                        return Ok(());
                    }
                }
            }
            _ => {}
        }
        let hint = match (&actual, expected) {
            (Type::None, Type::Ptr(..)) => format!("; a pointer can't be none, so use 'maybe {}'", expected),
            (Type::None, Type::Or(value, _)) if value.ptr_inner().is_some() => {
                format!("; a pointer can't be none, so use 'maybe {}'", value)
            }
            (_, Type::Or(_, error)) if fits_regions(&actual, error) => "; an error is given with 'fail', as in '<- fail e'".to_string(),
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
        };
        err(expr.span, format!("expected type {}, got {}{}", expected, actual, hint))
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
        (Type::Ptr(a, ar), Type::Ptr(e, er)) => region(ar, er) && fits_regions(a, e),
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
fn unify_regions(pattern: &Type, actual: &Type, regions: &mut HashMap<String, String>) {
    let mut record = |p: &Option<String>, a: &Option<String>| {
        if let (Some(p), Some(a)) = (p, a)
            && a != ANY_REGION
        {
            regions.entry(p.clone()).or_insert_with(|| a.clone());
        }
    };
    match (pattern, actual) {
        (Type::Ptr(p, pr), Type::Ptr(a, ar)) => {
            record(pr, ar);
            unify_regions(p, a, regions);
        }
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
        Type::Ptr(..) => true,
        Type::Named(name, _) => regional.contains(name),
        Type::Maybe(inner) => holds_pointers(inner, regional),
        Type::Or(value, error) => holds_pointers(value, regional) || holds_pointers(error, regional),
        _ => false,
    }
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
        Type::Maybe(inner) => contained_by_value(inner),
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
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Fail(inner) => named_types(inner),
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
        Type::Ptr(inner, _) => constructible(inner, can),
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

/// `1 argument`, `2 arguments`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {}", noun) } else { format!("{} {}s", n, noun) }
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
