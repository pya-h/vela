use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::check;
use crate::error::{Error, Result};

/// Accumulates C source line by line and tracks indentation. Appending to a
/// `String` cannot fail, so no method returns a `Result`.
#[derive(Default)]
struct CWriter {
    out: String,
    depth: usize,
}

impl CWriter {
    /// Writes `text` as a full line at the current indentation.
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    /// Writes `header {` (or just `{` for an empty header) and indents the
    /// lines that follow.
    fn open(&mut self, header: &str) {
        if header.is_empty() {
            self.line("{");
        } else {
            self.line(&format!("{} {{", header));
        }
        self.depth += 1;
    }

    /// Ends the innermost block with `}` followed by `trailer`, as in `} Point;`.
    fn close_with(&mut self, trailer: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.line(&format!("}}{}", trailer));
    }

    fn close(&mut self) {
        self.close_with("");
    }

    /// Ends the innermost block and opens the next one, as in `} else {`.
    fn reopen(&mut self, keyword: &str) {
        self.close_with(&format!(" {} {{", keyword));
        self.depth += 1;
    }
}

/// Generates C from a program that has already been checked by
/// `check::Checker`. Types come from the checker's annotations; the generator
/// does no type inference of its own. A missing annotation or other broken
/// checker guarantee is reported as an internal compiler error.
///
/// An expression is normally one C expression. One that contains a plan B
/// (`x { … }`), a change to a slice (`xs.del()`), or a `scan` needs statements, so
/// generating it writes those statements ("the prelude") before the line that
/// uses its value; see `gen_operands`.
///
/// A `region r` is an arena, `vela__rt__region r[1]`: as an array, `r` is a
/// pointer to it, like a region parameter. Every block frees the regions it
/// declared when it closes, and a `<-` frees every region the task declared
/// before it returns.
pub struct Generator {
    w: CWriter,
    temp_counter: u64,
    kinds: HashMap<String, KindDef>,
    shapes: HashMap<String, ShapeDef>,
    /// The shapes and kinds holding text or pointers, whose values a copy
    /// goes through (see `check::regional_types` and `check::textual_types`).
    copied_through: HashSet<String>,
    /// The return type of the task being generated.
    ret: Type,
    /// The regions declared in each block being generated, outermost first.
    regions: Vec<Vec<String>>,
    /// The names of the task's results, in order, when it names them.
    results: Vec<String>,
    /// The name of the task being generated, for runtime messages.
    task: String,
    /// The methods that change the value they are called on, which take it by
    /// pointer (see `signature`), by the name of the task each is.
    altering: HashSet<String>,
    /// Whether the task being generated is one of them, so that the value it is
    /// called on is read through its pointer.
    alters: bool,
    /// The C names of the regions the `show` calls being generated make for the
    /// values written by their own `str`, innermost last (see `gen_show`). A
    /// `show` nests inside another only through a plan B of one of its values.
    shown: Vec<String>,
}

impl Default for Generator {
    fn default() -> Self {
        Generator {
            w: CWriter::default(),
            temp_counter: 0,
            kinds: HashMap::new(),
            shapes: HashMap::new(),
            copied_through: HashSet::new(),
            ret: Type::Unit,
            regions: Vec::new(),
            results: Vec::new(),
            task: String::new(),
            altering: HashSet::new(),
            alters: false,
            shown: Vec::new(),
        }
    }
}

impl Generator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generate(&mut self, items: &[Item]) -> Result<String> {
        self.kinds.clear();
        self.shapes.clear();
        self.altering.clear();
        for item in items {
            if let Item::Task(t) = item
                && t.owner.as_ref().is_some_and(|owner| owner.alters)
            {
                // One task is generated for each set of types a method of a
                // shape that takes them is used with, and each is an item of
                // its own, so every one of them is listed here.
                self.altering.insert(t.name.clone());
            }
            match item {
                Item::Kind(k) => {
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                // A shape that takes types generates nothing: the shape made
                // for each set of types it is given is generated in its place
                // (see the checker's `shape_instance`).
                Item::Shape(s) if s.types.is_empty() => {
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Shape(_) => {}
                Item::Task(_) => {}
            }
        }
        self.copied_through = check::regional_types(items);
        self.copied_through.extend(check::textual_types(items));
        let used = UsedTypes::collect(items);

        self.w = CWriter::default();
        self.w.line("#include \"vela_rt.h\"");
        self.w.blank();

        // Types. A typedef for every shape, kind, `maybe`, and `or` comes first,
        // so any type can point to any other (or to itself); then the kinds' tag
        // enums; then the struct bodies, each after the types it contains by
        // value.
        for item in items {
            if let Some(name) = type_name(item) {
                self.w.line(&format!("typedef struct {0} {0};", mangle(name)));
            }
        }
        for ty in &used.compounds {
            self.w.line(&format!("typedef struct {0} {0};", c_type(ty)));
        }
        for item in items {
            if let Item::Kind(k) = item {
                self.gen_tag_enum(k);
            }
        }
        for def in type_order(items, &used.compounds)? {
            match def {
                TypeDef::Item(Item::Shape(s)) => self.gen_shape(s),
                TypeDef::Item(Item::Kind(k)) => self.gen_kind(k),
                TypeDef::Item(Item::Task(_)) => {}
                TypeDef::Compound(ty) => self.gen_compound(ty),
            }
        }
        for ty in &used.written {
            self.gen_show_maybe(ty, false);
        }
        for ty in &used.shown {
            self.gen_show_maybe(ty, true);
        }
        self.gen_item_helpers(&used);
        self.gen_tuple_eq(&used.tuple_eq);
        self.gen_copies(&used.copied);
        self.w.blank();

        // Forward declarations for tasks. A generic task generates nothing: one
        // task per set of types a call gives it is generated in its place (see
        // the checker's `expand_instances`).
        for item in items {
            if let Item::Task(t) = item
                && t.types.is_empty()
            {
                self.w.line(&format!("{};", signature(t, self.altering.contains(&t.name))));
            }
        }
        self.w.blank();

        // Task definitions
        let mut has_main = false;
        for item in items {
            if let Item::Task(t) = item
                && t.types.is_empty()
            {
                if t.name == "main" {
                    has_main = true;
                }
                self.gen_task(t)?;
            }
        }

        if has_main {
            self.w.blank();
            self.w.open("int main(void)");
            // Line by line even into a pipe, so output isn't lost if the
            // program then crashes.
            self.w.line("setvbuf(stdout, NULL, _IOLBF, BUFSIZ);");
            self.w.line("vela_main();");
            self.w.line("return 0;");
            self.w.close();
        }

        Ok(std::mem::take(&mut self.w.out))
    }

    fn fresh_temp(&mut self, prefix: &str) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("vela__{}__{}", prefix, n)
    }

    /// A name for the region a `show` makes for its line (see `gen_show`). It
    /// goes where the regions a block declares go, and those are written through
    /// `c_ident`, which leaves this name as it is: no Vela name holds `__`, so
    /// no region of a program is called this.
    fn shown_region(&mut self) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("shown__{}", n)
    }

    fn gen_shape(&mut self, s: &ShapeDef) {
        self.w.open(&format!("struct {}", mangle(&s.name)));
        if s.fields.is_empty() {
            // ISO C has no empty structs.
            self.w.line("char vela__empty__;");
        }
        for field in &s.fields {
            self.w.line(&format!("{} {};", c_type(&field.ty), c_ident(&field.name)));
        }
        self.w.close_with(";");
    }

    fn gen_tag_enum(&mut self, k: &KindDef) {
        let tag_type = tag_type(&k.name);
        self.w.open(&format!("typedef enum {}", tag_type));
        for v in &k.variants {
            self.w.line(&format!("{},", tag_value(&k.name, &v.tag)));
        }
        self.w.close_with(&format!(" {};", tag_type));
    }

    fn gen_kind(&mut self, k: &KindDef) {
        self.w.open(&format!("struct {}", mangle(&k.name)));
        self.w.line(&format!("{} tag;", tag_type(&k.name)));
        // Variants without a payload have no union member, and a kind with no
        // payloads at all has no union (ISO C has no empty unions).
        if k.variants.iter().any(|v| !v.payload.is_empty()) {
            self.w.open("union");
            for v in &k.variants {
                if !v.payload.is_empty() {
                    self.w.open("struct");
                    for field in &v.payload {
                        self.w.line(&format!("{} {};", c_type(&field.ty), c_ident(&field.name)));
                    }
                    self.w.close_with(&format!(" {};", c_ident(&v.tag)));
                }
            }
            self.w.close_with(" data;");
        }
        self.w.close_with(";");
    }

    /// The struct of a `maybe T` (`has`, `value`), a `T or E` (`ok`, `value`,
    /// `error`; no `value` for `unit or E`), several results (`r0`, `r1`, …),
    /// an array (`items`), or a slice (`items`, a pointer, `len`, and `room`,
    /// where `add` puts more items; see `vela__rt__items_insert`). A
    /// `maybe`'s and an `or`'s members are always initialized, so either can be
    /// read.
    fn gen_compound(&mut self, ty: &Type) {
        self.w.open(&format!("struct {}", c_type(ty)));
        match ty {
            Type::Results(types) => {
                for (i, result) in types.iter().enumerate() {
                    self.w.line(&format!("{} {};", c_type(result), result_member(i)));
                }
            }
            Type::Tuple(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.w.line(&format!("{} {};", c_type(item), tuple_member(i)));
                }
            }
            Type::Maybe(inner) => {
                self.w.line("bool has;");
                self.w.line(&format!("{} value;", c_type(inner)));
            }
            Type::Array(items, count) => self.w.line(&format!("{} items[{}];", c_type(items), count)),
            Type::Slice(items, _) => {
                self.w.line(&format!("{}* items;", c_type(items)));
                self.w.line("int64_t len;");
                self.w.line("vela__rt__room* room;");
            }
            Type::Or(value, error) => {
                self.w.line("bool ok;");
                if **value != Type::Unit {
                    self.w.line(&format!("{} value;", c_type(value)));
                }
                self.w.line(&format!("{} error;", c_type(error)));
            }
            _ => {}
        }
        self.w.close_with(";");
    }

    /// The helpers for arrays and slices that the program uses, each a C
    /// function so that an array or slice given to it is evaluated once.
    fn gen_item_helpers(&mut self, used: &UsedTypes) {
        for slice in &used.slice_at {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {}* {}({} s, int64_t i, const char* at)", items, item_helper("at", slice), ctype));
            self.w.line("return s.items + vela__rt__index(i, s.len, at);");
            self.w.close();
        }
        for slice in &used.slice_part {
            let ctype = c_type(slice);
            self.w.open(&format!("static {0} {1}({0} s, int64_t from, int64_t to, const char* at)", ctype, item_helper("part", slice)));
            self.w.line("vela__rt__part(from, to, s.len, \"slice\", at);");
            // A part ending where the slice's room is used up can grow into it.
            self.w.line(&format!("return ({}){{ .items = s.items + from, .len = to - from, .room = s.room }};", ctype));
            self.w.close();
        }
        for slice in &used.slice_copy {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, {0} s)", ctype, item_helper("copy", slice)));
            // The source exists, so its size in bytes can't overflow.
            self.w.line(&format!("{0}* items = vela__rt__region_alloc(r, (size_t)s.len * sizeof({0}), _Alignof({0}));", items));
            self.w.line(&format!("memcpy(items, s.items, (size_t)s.len * sizeof({}));", items));
            self.w.line(&format!("return ({}){{ .items = items, .len = s.len }};", ctype));
            self.w.close();
        }
        for slice in &used.slice_fill {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, int64_t len, {2} value, const char* at)", ctype, item_helper("fill", slice), items));
            self.w.line(&format!("{0}* items = vela__rt__region_items(r, len, sizeof({0}), _Alignof({0}), at);", items));
            self.w.open("for (int64_t i = 0; i < len; i++)");
            self.w.line("items[i] = value;");
            self.w.close();
            self.w.line(&format!("return ({}){{ .items = items, .len = len }};", ctype));
            self.w.close();
        }
        for slice in &used.slice_add {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static void {}(vela__rt__region* r, {}* s, {} value)", item_helper("add", slice), ctype, items));
            self.w.line(&format!("{0}* items = vela__rt__items_insert(r, s->items, s->len, &s->room, s->len, sizeof({0}), _Alignof({0}));", items));
            self.w.line("items[s->len] = value;");
            self.w.line("s->items = items;");
            self.w.line("s->len += 1;");
            self.w.close();
        }
        for slice in &used.slice_insert {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static void {}(vela__rt__region* r, {}* s, int64_t i, {} value, const char* at)", item_helper("insert", slice), ctype, items));
            // Any index from 0 to the length: the item goes before item `i`, or last.
            self.w.line("vela__rt__index(i, s->len + 1, at);");
            self.w.line(&format!("{0}* items = vela__rt__items_insert(r, s->items, s->len, &s->room, i, sizeof({0}), _Alignof({0}));", items));
            self.w.line("items[i] = value;");
            self.w.line("s->items = items;");
            self.w.line("s->len += 1;");
            self.w.close();
        }
        for slice in &used.slice_del {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}({2}* s, int64_t i, const char* at)", items, item_helper("del", slice), ctype));
            self.w.line(&format!("{} item = s->items[vela__rt__index(i, s->len, at)];", items));
            self.w.line(&format!("vela__rt__items_remove(s->items, s->len, s->room, i, sizeof({}));", items));
            self.w.line("s->len -= 1;");
            self.w.line("return item;");
            self.w.close();
        }
        for slice in &used.slice_pop {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}({2}* s, const char* at)", items, item_helper("pop", slice), ctype));
            self.w.line("vela__rt__not_empty(s->len, at);");
            self.w.line(&format!("{} item = s->items[s->len - 1];", items));
            self.w.line(&format!("vela__rt__items_remove(s->items, s->len, s->room, s->len - 1, sizeof({}));", items));
            self.w.line("s->len -= 1;");
            self.w.line("return item;");
            self.w.close();
        }
        if used.scans {
            let maybe = c_type(&Type::Maybe(Box::new(Type::Text(None))));
            self.w.open(&format!("static {} vela__scan(vela__rt__region* r, vela__rt__text prompt)", maybe));
            self.w.line(&format!("{} line = {{ .has = false }};", maybe));
            self.w.line("line.has = vela__rt__scan(r, prompt, &line.value);");
            self.w.line("return line;");
            self.w.close();
        }
        for array in &used.array_fill {
            let Type::Array(items, count) = array else { continue };
            let ctype = c_type(array);
            self.w.open(&format!("static {0} {1}({2} value)", ctype, item_helper("fill", array), c_type(items)));
            self.w.line(&format!("{} array;", ctype));
            self.w.open(&format!("for (int64_t i = 0; i < INT64_C({}); i++)", count));
            self.w.line("array.items[i] = value;");
            self.w.close();
            self.w.line("return array;");
            self.w.close();
        }
    }

    /// `vela__eq__K` for each tuple type compared with `==` or `!=`: whether
    /// two tuples' items are equal, item by item.
    fn gen_tuple_eq(&mut self, tuples: &[Type]) {
        for tuple in tuples {
            let Type::Tuple(items) = tuple else { continue };
            self.w.open(&format!("static bool {0}({1} a, {1} b)", copy_helper("eq", tuple), c_type(tuple)));
            let tests: Vec<String> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let (a, b) = (format!("a.{}", tuple_member(i)), format!("b.{}", tuple_member(i)));
                    match item {
                        Type::Text(_) => format!("vela__rt__text_eq({}, {})", a, b),
                        Type::Tuple(_) => format!("{}({}, {})", copy_helper("eq", item), a, b),
                        _ => format!("{} == {}", a, b),
                    }
                })
                .collect();
            self.w.line(&format!("return {};", tests.join(" && ")));
            self.w.close();
        }
    }

    /// For each type a program copies (`x.copy() at r`), `vela__copied__K`, which
    /// copies a value into a region; and for each type a copy goes through,
    /// `vela__fix__K`, which replaces what a value copied byte for byte holds
    /// in regions with copies (see `vela__rt__copy` in the runtime).
    fn gen_copies(&mut self, copied: &[Type]) {
        let mut fixed: Vec<Type> = Vec::new();
        let mut waiting: Vec<Type> = copied.iter().map(fix_type).collect();
        while let Some(ty) = waiting.pop() {
            if fixed.contains(&ty) || !self.needs_fix(&ty) {
                continue;
            }
            waiting.extend(self.fix_parts(&ty).into_iter().map(|part| fix_type(&part)));
            fixed.push(ty);
        }
        for ty in &fixed {
            self.w.line(&format!("static void {}(vela__rt__copy* c, void* at);", copy_helper("fix", ty)));
        }
        for ty in &fixed {
            self.gen_fix(ty);
        }
        for ty in copied {
            if !self.needs_fix(&fix_type(ty)) {
                continue;
            }
            let ctype = c_type(ty);
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, {0} value)", ctype, copy_helper("copied", ty)));
            self.w.line("vela__rt__copy c = { .r = r };");
            self.w.line(&format!("{}(&c, &value);", copy_helper("fix", &fix_type(ty))));
            self.w.line("vela__rt__copy_finish(&c);");
            self.w.line("return value;");
            self.w.close();
        }
    }

    /// Whether a value of type `ty` holds anything in a region that a copy
    /// replaces: text, a slice, or a pointer, directly or by value.
    fn needs_fix(&self, ty: &Type) -> bool {
        match ty {
            Type::Text(_) | Type::Ptr(..) | Type::Slice(..) => true,
            Type::Named(name, _) => self.copied_through.contains(name),
            Type::Maybe(inner) | Type::Array(inner, _) => self.needs_fix(inner),
            Type::Or(value, error) => self.needs_fix(value) || self.needs_fix(error),
            Type::Tuple(items) => items.iter().any(|item| self.needs_fix(item)),
            _ => false,
        }
    }

    /// The types whose values a value of type `ty` holds, directly or behind
    /// a pointer or slice, which its fix may need to fix too.
    fn fix_parts(&self, ty: &Type) -> Vec<Type> {
        match ty {
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) => vec![(**inner).clone()],
            Type::Or(value, error) => vec![(**value).clone(), (**error).clone()],
            Type::Tuple(items) => items.clone(),
            Type::Named(name, _) => match (self.shapes.get(name), self.kinds.get(name)) {
                (Some(shape), _) => shape.fields.iter().map(|f| f.ty.without_regions()).collect(),
                (_, Some(kind)) => kind.variants.iter().flat_map(|v| &v.payload).map(|f| f.ty.without_regions()).collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// `vela__fix__K` for `ty`, which `needs_fix`.
    fn gen_fix(&mut self, ty: &Type) {
        let ctype = c_type(ty);
        self.w.open(&format!("static void {}(vela__rt__copy* c, void* at)", copy_helper("fix", ty)));
        self.w.line(&format!("{0}* v = at;", ctype));
        match ty {
            Type::Text(_) => self.w.line("*v = vela__rt__copy_text(c, *v);"),
            Type::Ptr(inner, _) => {
                let fix = if self.needs_fix(&fix_type(inner)) { copy_helper("fix", &fix_type(inner)) } else { "NULL".to_string() };
                let pointee = c_type(inner);
                self.w.line(&format!("*v = vela__rt__copy_pointer(c, *v, sizeof({0}), _Alignof({0}), {1});", pointee, fix));
            }
            Type::Slice(inner, _) => {
                let items = c_type(inner);
                self.w.line(&format!("v->items = vela__rt__copy_items(c, v->items, v->len, sizeof({0}), _Alignof({0}));", items));
                self.w.line("v->room = NULL;");
                if self.needs_fix(&fix_type(inner)) {
                    self.w.open("for (int64_t i = 0; i < v->len; i++)");
                    self.w.line(&format!("{}(c, &v->items[i]);", copy_helper("fix", &fix_type(inner))));
                    self.w.close();
                }
            }
            Type::Array(inner, count) => {
                self.w.open(&format!("for (int64_t i = 0; i < INT64_C({}); i++)", count));
                self.w.line(&format!("{}(c, &v->items[i]);", copy_helper("fix", &fix_type(inner))));
                self.w.close();
            }
            Type::Maybe(inner) => {
                self.w.open("if (v->has)");
                self.w.line(&format!("{}(c, &v->value);", copy_helper("fix", &fix_type(inner))));
                self.w.close();
            }
            Type::Or(value, error) => {
                let fix_value = **value != Type::Unit && self.needs_fix(value);
                if fix_value {
                    self.w.open("if (v->ok)");
                    self.w.line(&format!("{}(c, &v->value);", copy_helper("fix", &fix_type(value))));
                }
                if self.needs_fix(error) {
                    if fix_value {
                        self.w.reopen("else");
                    } else {
                        self.w.open("if (!v->ok)");
                    }
                    self.w.line(&format!("{}(c, &v->error);", copy_helper("fix", &fix_type(error))));
                }
                self.w.close();
            }
            Type::Tuple(items) => {
                let fixed: Vec<(usize, &Type)> = items.iter().enumerate().filter(|(_, item)| self.needs_fix(item)).collect();
                for (i, item) in fixed {
                    self.w.line(&format!("{}(c, &v->{});", copy_helper("fix", &fix_type(item)), tuple_member(i)));
                }
            }
            Type::Named(name, _) => {
                if let Some(shape) = self.shapes.get(name).cloned() {
                    let fields: Vec<&Field> = shape.fields.iter().filter(|f| self.needs_fix(&fix_type(&f.ty))).collect();
                    for field in fields {
                        self.w.line(&format!("{}(c, &v->{});", copy_helper("fix", &fix_type(&field.ty)), c_ident(&field.name)));
                    }
                } else if let Some(kind) = self.kinds.get(name).cloned() {
                    self.w.open("switch (v->tag)");
                    for variant in &kind.variants {
                        let fields: Vec<&Field> = variant.payload.iter().filter(|f| self.needs_fix(&fix_type(&f.ty))).collect();
                        if fields.is_empty() {
                            continue;
                        }
                        self.w.open(&format!("case {}:", tag_value(name, &variant.tag)));
                        for field in fields {
                            self.w.line(&format!("{}(c, &v->data.{}.{});", copy_helper("fix", &fix_type(&field.ty)), c_ident(&variant.tag), c_ident(&field.name)));
                        }
                        self.w.line("break;");
                        self.w.close();
                    }
                    self.w.line("default:");
                    self.w.line("    break;");
                    self.w.close();
                }
            }
            _ => {}
        }
        self.w.close();
    }

    /// `show` for a `maybe` of a type `show` prints: the value, or `none`,
    /// ending the line when `ends`, as the runtime's helpers do for the types it
    /// writes itself (see `written_helper`).
    fn gen_show_maybe(&mut self, ty: &Type, ends: bool) {
        let Type::Maybe(inner) = ty else { return };
        self.w.open(&format!("static void {}({} m)", maybe_helper(ty, ends), c_type(ty)));
        self.w.open("if (m.has)");
        self.w.line(&format!("{}(m.value);", written_helper(inner, ends)));
        self.w.reopen("else");
        self.w.line(&format!("vela__rt__{}_none();", if ends { "show" } else { "write" }));
        self.w.close();
        self.w.close();
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        self.ret = t.ret.clone();
        self.alters = self.altering.contains(&t.name);
        self.regions.clear();
        self.results = t.result_names().map(|(name, _)| name.clone()).collect();
        // A method is named in a runtime message as the program writes it,
        // through the shape or kind it belongs to.
        self.task = match method_parts(&t.name) {
            Some((owner, method)) if t.owner.is_some() => format!("{}{}{}", owner, MODULE_SEPARATOR, method),
            _ => t.name.clone(),
        };
        self.w.open(&signature(t, self.alters));
        for param in &t.params {
            self.use_if_unread(&param.name, &t.body);
        }
        // Named results are locals of the task. The checker has made sure none
        // is read before it is set; the zero is there so that a checker of
        // mine that ever missed one would still read a defined value.
        for (result, ty) in t.results.iter().zip(t.ret.results()) {
            if let Some((name, _)) = &result.name {
                self.w.line(&format!("{} {} = {{0}};", c_type(ty), c_ident(name)));
            }
        }
        self.gen_block(&t.body)?;
        match &t.ret {
            Type::Unit => self.w.line("return;"),
            Type::Or(value, _) if **value == Type::Unit => {
                self.w.line(&format!("return {};", success(&t.ret)));
            }
            // Reaching the end of a task sends its results as they are.
            _ if !self.results.is_empty() => {
                let results = self.current_results();
                self.w.line(&format!("return {};", results));
            }
            _ => self.w.line(&format!("vela__rt__missing_send(\"{}\");", escape_c_string(&self.task))),
        }
        self.w.close();
        self.results.clear();
        Ok(())
    }

    /// The C expression for the task's named results as they are now: the one
    /// result, or a result struct holding them all.
    fn current_results(&self) -> String {
        let names: Vec<String> = self.results.iter().map(|name| c_ident(name)).collect();
        match names.as_slice() {
            [only] => only.clone(),
            _ => {
                let members: Vec<String> = names.iter().enumerate().map(|(i, name)| format!(".{} = {}", result_member(i), name)).collect();
                format!("(({}){{ {} }})", c_type(&self.ret), members.join(", "))
            }
        }
    }

    fn gen_block(&mut self, block: &[Stmt]) -> Result<()> {
        self.enter_block();
        self.gen_statements(block, block)?;
        self.leave_block(block);
        Ok(())
    }

    /// Starts a block that may declare regions.
    fn enter_block(&mut self) {
        self.regions.push(Vec::new());
    }

    /// Ends the block `block`, freeing the regions it declared, newest first.
    /// (After a final `<-`, which frees them itself, nothing more runs.)
    fn leave_block(&mut self, block: &[Stmt]) {
        let declared = self.regions.pop().unwrap_or_default();
        if matches!(block.last().map(|stmt| &stmt.kind), Some(StmtKind::Send(_))) {
            return;
        }
        for region in declared.iter().rev() {
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
    }

    /// Frees every region the task has declared so far, newest first, before
    /// a `<-` leaves it. Whether any exist.
    fn free_all_regions(&mut self) -> bool {
        let all: Vec<String> = self.regions.iter().flatten().cloned().collect();
        for region in all.iter().rev() {
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
        !all.is_empty()
    }

    /// Generates `stmts`, the first statements of `scope`: a variable declared
    /// in them counts as read if anything after it in `scope` reads it.
    fn gen_statements(&mut self, stmts: &[Stmt], scope: &[Stmt]) -> Result<()> {
        for (i, stmt) in stmts.iter().enumerate() {
            self.gen_stmt(stmt)?;
            if let StmtKind::Binding { names, .. } = &stmt.kind {
                for bound in names.iter().flat_map(Bound::leaves).filter(|bound| bound.name != DISCARD) {
                    self.use_if_unread(&bound.name, scope.get(i + 1..).unwrap_or_default());
                }
            }
        }
        Ok(())
    }

    /// Vela allows variables that are never read, but C compilers warn about
    /// them (`-Wunused-variable`, `-Wunused-but-set-variable`,
    /// `-Wunused-parameter`). If nothing in `scope` reads `name`, this writes
    /// `(void)name;`, which counts as a use in C.
    fn use_if_unread(&mut self, name: &str, scope: &[Stmt]) {
        if !block_reads(scope, name) {
            self.w.line(&format!("(void){};", c_ident(name)));
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match &stmt.kind {
            // A change to a slice drops the item `del` gives, and a `scan` the line.
            StmtKind::Expr(Expr { kind: ExprKind::Edit { items, op, args }, span, .. }) => {
                self.gen_edit(*span, items, *op, args, true)?;
            }
            StmtKind::Expr(Expr { kind: ExprKind::Scan { args, region, .. }, span, .. }) => {
                self.gen_scan(*span, args, region, true)?;
            }
            StmtKind::Expr(e) => {
                let expr = self.gen_expr(e)?;
                // A plan B that gives no value is all prelude, and one that
                // does gives a temporary, which the statement drops.
                if matches!(e.kind, ExprKind::PlanB { .. }) && !expr.is_empty() {
                    self.w.line(&format!("(void){};", expr));
                } else if !expr.is_empty() {
                    self.w.line(&format!("{};", expr));
                }
            }
            StmtKind::Binding { names, init, .. } => {
                let values = self.gen_value_list(names.len(), init, stmt.span)?;
                for (bound, value) in names.iter().zip(values) {
                    let ty = bound
                        .ty
                        .as_ref()
                        .ok_or_else(|| Error::internal_at(bound.span, format!("binding '{}' has no type", bound.name)))?;
                    if !bound.parts.is_empty() && bound.leaves().iter().all(|leaf| leaf.name == DISCARD) {
                        // `(_, _)`: nothing to declare.
                        self.drop_value(&value);
                    } else if !bound.parts.is_empty() {
                        // The tuple is kept once, and each name takes its item.
                        let tuple = self.fresh_temp("tuple");
                        self.w.line(&format!("{} {} = {};", c_type(ty), tuple, value));
                        self.gen_unpacked(bound, &tuple)?;
                    } else if bound.name == DISCARD {
                        // Nothing to declare: the value is computed and dropped.
                        self.drop_value(&value);
                    } else {
                        self.w.line(&format!("{} {} = {};", c_type(ty), c_ident(&bound.name), value));
                    }
                }
            }
            StmtKind::Assign(targets, values) => {
                // The checker guarantees every place is a variable, field, or
                // deref, all of which generate valid C lvalues.
                if let ([Target::Place(lhs)], [rhs]) = (targets.as_slice(), values.as_slice()) {
                    if has_prelude(rhs) && !matches!(lhs.kind, ExprKind::Var(_)) {
                        // Find the place before the value's prelude runs, which
                        // might change what the place refers to.
                        let place = self.gen_place(lhs)?;
                        let pointer = self.fresh_temp("place");
                        self.w.line(&format!("{}* {} = &{};", c_type(lhs.ty()?), pointer, place));
                        let value = self.gen_expr(rhs)?;
                        self.w.line(&format!("*{} = {};", pointer, value));
                    } else {
                        let l = self.gen_place(lhs)?;
                        let r = self.gen_expr(rhs)?;
                        self.w.line(&format!("{} = {};", l, r));
                    }
                } else {
                    self.gen_multi_assign(targets, values, stmt.span)?;
                }
            }
            StmtKind::When(cond, then_b, else_b) => {
                match &cond.pattern {
                    None => {
                        let c = self.gen_condition(&cond.value)?;
                        self.w.open(&format!("if ({})", c));
                        self.gen_block(then_b)?;
                    }
                    Some(pattern) => {
                        let subject = self.gen_subject(&cond.value)?;
                        let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                        self.w.open(&format!("if ({})", test));
                        self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, then_b)?;
                        self.gen_block(then_b)?;
                    }
                }
                if let Some(else_b) = else_b {
                    self.w.reopen("else");
                    self.gen_block(else_b)?;
                }
                self.w.close();
            }
            StmtKind::Loop(cond, body) => match &cond.pattern {
                None if !has_prelude(&cond.value) => {
                    let c = self.gen_condition(&cond.value)?;
                    self.w.open(&format!("while ({})", c));
                    self.gen_block(body)?;
                    self.w.close();
                }
                None => {
                    // The condition's prelude runs before every test.
                    self.w.open("while (1)");
                    let c = self.gen_expr(&cond.value)?;
                    self.w.open(&format!("if (!{})", parenthesized(&c)));
                    self.w.line("break;");
                    self.w.close();
                    self.gen_block(body)?;
                    self.w.close();
                }
                Some(pattern) => {
                    self.w.open("while (1)");
                    let subject = self.gen_subject(&cond.value)?;
                    let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                    self.w.open(&format!("if (!({}))", test));
                    self.w.line("break;");
                    self.w.close();
                    self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, body)?;
                    self.gen_block(body)?;
                    self.w.close();
                }
            },
            StmtKind::Each(name, items, body) if !matches!(items.kind, ExprKind::Range(..)) => {
                // An array is copied, so the loop sees it as it was when it
                // started; a slice is a view, whose items are read as they are.
                let ty = items.ty()?.clone();
                let (item, len) = match &ty {
                    Type::Array(item, count) => (item, format!("INT64_C({})", count)),
                    Type::Slice(item, _) => (item, String::new()),
                    other => return Err(Error::internal_at(items.span, format!("each over a value of type {}", other))),
                };
                let value = self.gen_expr(items)?;
                let walked = self.fresh_temp("items");
                let index = self.fresh_temp("index");
                let len = if len.is_empty() { format!("{}.len", walked) } else { len };
                self.w.open("");
                self.w.line(&format!("{} {} = {};", c_type(&ty), walked, value));
                self.w.open(&format!("for (int64_t {0} = 0; {0} < {1}; {0}++)", index, len));
                self.w.line(&format!("{} {} = {}.items[{}];", c_type(item), c_ident(name), walked, index));
                self.use_if_unread(name, body);
                self.gen_block(body)?;
                self.w.close();
                self.w.close();
            }
            StmtKind::Each(name, range, body) => {
                let (a, b) = self.gen_range(range)?;
                // The end of the range is evaluated once, before the first
                // iteration. (The checker forbids shadowing, so `b` cannot
                // refer to a different variable called `name`.)
                let end = self.fresh_temp("end");
                let name = c_ident(name);
                self.w.open(&format!(
                    "for (int64_t {} = {}, {} = {}; {} < {}; {}++)",
                    name, a, end, b, name, end, name
                ));
                self.gen_block(body)?;
                self.w.close();
            }
            StmtKind::Match(expr, cases) => self.gen_match(expr, cases)?,
            StmtKind::Send(values) if values.is_empty() => {
                // A task's own regions are freed first; the results are locals,
                // and can't point into one of them.
                let results = (!self.results.is_empty()).then(|| self.current_results());
                self.free_all_regions();
                match (results, &self.ret) {
                    (Some(results), _) => self.w.line(&format!("return {};", results)),
                    (None, Type::Or(..)) => self.w.line(&format!("return {};", success(&self.ret))),
                    (None, _) => self.w.line("return;"),
                }
            }
            StmtKind::SetResults(names, values) => {
                let given = self.gen_value_list(names.len(), values, stmt.span)?;
                for ((name, _), value) in names.iter().zip(given) {
                    self.w.line(&format!("{} = {};", c_ident(name), value));
                }
            }
            StmtKind::Send(values) => {
                let expr = self.gen_sent_value(values, stmt.span)?;
                if self.regions.iter().all(Vec::is_empty) {
                    self.w.line(&format!("return {};", expr));
                } else {
                    // The value may read memory in the regions, so it is
                    // computed before they are freed.
                    let result = self.fresh_temp("result");
                    self.w.line(&format!("{} {} = {};", c_type(&self.ret), result, expr));
                    self.free_all_regions();
                    self.w.line(&format!("return {};", result));
                }
            }
            StmtKind::Region(name) => {
                self.w.line(&format!("vela__rt__region {}[1] = {{ VELA__RT__REGION_EMPTY }};", c_ident(name)));
                if let Some(block) = self.regions.last_mut() {
                    block.push(name.clone());
                }
            }
        }
        Ok(())
    }

    /// The C expressions for `wanted` values received by a list of names or
    /// places: one per value, or the members of the result struct of a single
    /// call that sends them all. Values are evaluated in order, before the
    /// first of them is stored, so `a, b = b, a` swaps.
    fn gen_value_list(&mut self, wanted: usize, values: &[Expr], span: Span) -> Result<Vec<String>> {
        if values.len() == wanted {
            let operands: Vec<&Expr> = values.iter().collect();
            let mut given = self.gen_operands(&operands)?;
            if wanted > 1 {
                // Each value is stored before any of them is, since a place
                // assigned here may be read by a later value.
                for (value, expr) in given.iter_mut().zip(values) {
                    if matches!(expr.ty()?, Type::Unit) {
                        // A call that sends nothing, received by `_`: it runs here
                        // (or already has, as a prelude, when it wrote nothing).
                        if !value.is_empty() {
                            self.w.line(&format!("{};", value));
                        }
                        value.clear();
                        continue;
                    }
                    let temp = self.fresh_temp("value");
                    self.w.line(&format!("{} {} = {};", c_type(expr.ty()?), temp, value));
                    *value = temp;
                }
            }
            return Ok(given);
        }
        // One call giving every value, or one tuple giving its items.
        let [call] = values else {
            return Err(Error::internal_at(span, format!("{} values received by {} names", values.len(), wanted)));
        };
        let ty = call.ty()?.clone();
        let (count, member): (usize, fn(usize) -> String) = match &ty {
            Type::Results(types) => (types.len(), result_member),
            Type::Tuple(items) => (items.len(), tuple_member),
            other => return Err(Error::internal_at(span, format!("a value of type {} received by {} names", other, wanted))),
        };
        let value = self.gen_expr(call)?;
        let results = self.fresh_temp("results");
        self.w.line(&format!("{} {} = {};", c_type(&ty), results, value));
        Ok((0..count).map(|i| format!("{}.{}", results, member(i))).collect())
    }

    /// Declares the names `( … )` takes from the tuple `tuple`, a C expression
    /// read without effects.
    fn gen_unpacked(&mut self, group: &Bound, tuple: &str) -> Result<()> {
        for (i, part) in group.parts.iter().enumerate() {
            let item = format!("{}.{}", tuple, tuple_member(i));
            if !part.parts.is_empty() {
                self.gen_unpacked(part, &item)?;
            } else if part.name != DISCARD {
                let ty = part.ty.as_ref().ok_or_else(|| Error::internal_at(part.span, format!("binding '{}' has no type", part.name)))?;
                self.w.line(&format!("{} {} = {};", c_type(ty), c_ident(&part.name), item));
            }
        }
        Ok(())
    }

    /// The pointers to the places `target` writes to, with how each is read from
    /// the value given (a member path of tuples, empty for the value itself),
    /// found before any value is computed.
    fn gen_target_places(&mut self, target: &Target, path: String, places: &mut Vec<(Option<String>, String)>) -> Result<()> {
        match target {
            Target::Discard(_) => places.push((None, path)),
            Target::Place(place) => {
                let lvalue = self.gen_place(place)?;
                let pointer = self.fresh_temp("place");
                self.w.line(&format!("{}* {} = &{};", c_type(place.ty()?), pointer, lvalue));
                places.push((Some(pointer), path));
            }
            Target::Tuple(parts, _) => {
                for (i, part) in parts.iter().enumerate() {
                    self.gen_target_places(part, format!("{}.{}", path, tuple_member(i)), places)?;
                }
            }
        }
        Ok(())
    }

    /// Writes `value`, which is received by a `_`, as a statement that drops it.
    fn drop_value(&mut self, value: &str) {
        if !value.is_empty() {
            self.w.line(&format!("(void){};", parenthesized(value)));
        }
    }

    /// An assignment to several places, or to a single `_`.
    fn gen_multi_assign(&mut self, targets: &[Target], values: &[Expr], span: Span) -> Result<()> {
        // Every place is found before the values run, since a value may change
        // what a later place refers to, and a place assigned here may be read
        // by a later value.
        let mut places = Vec::new();
        for target in targets {
            let mut parts = Vec::new();
            self.gen_target_places(target, String::new(), &mut parts)?;
            places.push(parts);
        }
        let types = given_types(targets.len(), values, span)?;
        let given = self.gen_value_list(targets.len(), values, span)?;
        for (((parts, target), value), ty) in places.into_iter().zip(targets).zip(given).zip(types) {
            // A tuple taken apart is kept once, and each place takes its item.
            let value = match target {
                Target::Tuple(..) if parts.iter().all(|(place, _)| place.is_none()) => {
                    // `(_, _)`: nothing is written.
                    self.drop_value(&value);
                    continue;
                }
                Target::Tuple(..) => {
                    let tuple = self.fresh_temp("tuple");
                    self.w.line(&format!("{} {} = {};", c_type(&ty), tuple, value));
                    tuple
                }
                _ => value,
            };
            for (place, path) in parts {
                match place {
                    Some(pointer) => self.w.line(&format!("*{} = {}{};", pointer, value, path)),
                    None if path.is_empty() => self.drop_value(&value),
                    None => {}
                }
            }
        }
        Ok(())
    }

    /// The one C expression a `<-` sends: the value, or a result struct holding
    /// every value.
    fn gen_sent_value(&mut self, values: &[Expr], span: Span) -> Result<String> {
        if let [only] = values {
            return self.gen_expr(only);
        }
        let Type::Results(types) = self.ret.clone() else {
            return Err(Error::internal_at(span, format!("{} values sent by a task of type {}", values.len(), self.ret)));
        };
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        let members: Vec<String> = given.iter().enumerate().map(|(i, value)| format!(".{} = {}", result_member(i), value)).collect();
        Ok(format!("(({}){{ {} }})", c_type(&Type::Results(types)), members.join(", ")))
    }

    /// The value a `match` or `is` tests, as a C expression that can be
    /// read several times: a variable is used directly, anything else is
    /// stored in a temporary first.
    fn gen_subject(&mut self, expr: &Expr) -> Result<String> {
        let value = self.gen_expr(expr)?;
        if matches!(expr.kind, ExprKind::Var(_)) {
            return Ok(value);
        }
        let subject = self.fresh_temp("subject");
        self.w.line(&format!("{} {} = {};", c_type(expr.ty()?), subject, value));
        Ok(subject)
    }

    /// The C condition under which `pattern` matches `subject`, of type `ty`.
    fn pattern_test(&self, ty: &Type, subject: &str, pattern: &Pattern) -> Result<String> {
        let test = match (&pattern.kind, ty) {
            (PatternKind::Any, _) => "1".to_string(),
            (PatternKind::Value(_), Type::Maybe(inner)) if is_pointer(inner) => format!("{} != NULL", subject),
            (PatternKind::None, Type::Maybe(inner)) if is_pointer(inner) => format!("{} == NULL", subject),
            (PatternKind::Value(_), Type::Maybe(_)) => format!("{}.has", subject),
            (PatternKind::None, Type::Maybe(_)) => format!("!{}.has", subject),
            (PatternKind::Value(_), Type::Or(..)) => format!("{}.ok", subject),
            (PatternKind::Fail(_), Type::Or(..)) => format!("!{}.ok", subject),
            (PatternKind::Variant { tag, .. }, Type::Named(kind, _)) => format!("{}.tag == {}", subject, tag_value(kind, tag)),
            // A tuple's items always match; the names take them.
            (PatternKind::Tuple(_), _) => "1".to_string(),
            _ => return Err(Error::internal_at(pattern.span, format!("pattern '{}' on type {}", pattern.label(), ty))),
        };
        Ok(test)
    }

    /// Declares the names `pattern` binds from `subject`, of type `ty`, for the
    /// block `body`.
    fn gen_pattern_bindings(&mut self, ty: &Type, subject: &str, pattern: &Pattern, body: &[Stmt]) -> Result<()> {
        let mut bindings: Vec<(&str, Type, String)> = Vec::new();
        let mut named = false;
        match (&pattern.kind, ty) {
            (PatternKind::Value(names), Type::Maybe(inner)) if is_pointer(inner) => {
                bound_names(names, inner, subject.to_string(), &mut bindings);
            }
            (PatternKind::Value(names), Type::Maybe(inner) | Type::Or(inner, _)) => {
                bound_names(names, inner, format!("{}.value", subject), &mut bindings);
            }
            (PatternKind::Fail(names), Type::Or(_, error)) => {
                bound_names(names, error, format!("{}.error", subject), &mut bindings);
            }
            // `(x, y)`: the items of the tuple itself.
            (PatternKind::Tuple(names), _) => bound_names(names, ty, subject.to_string(), &mut bindings),
            (PatternKind::Variant { tag, binds, .. }, Type::Named(kind_name, _)) => {
                let variant = self
                    .kinds
                    .get(kind_name)
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .ok_or_else(|| Error::internal_at(pattern.span, format!("unknown variant '{}'", tag)))?;
                if binds.len() != variant.payload.len() {
                    return Err(Error::internal_at(
                        pattern.span,
                        format!("case '{}' has {} bindings (expected {})", tag, binds.len(), variant.payload.len()),
                    ));
                }
                for (names, field) in binds.iter().zip(&variant.payload) {
                    let value = format!("{}.data.{}.{}", subject, c_ident(tag), c_ident(&field.name));
                    bound_names(names, &field.ty, value, &mut bindings);
                }
            }
            _ => {}
        }
        for (name, ty, value) in bindings {
            if name == DISCARD {
                continue;
            }
            named = true;
            self.w.line(&format!("{} {} = {};", c_type(&ty), c_ident(name), value));
            // A pattern must name every value, even ones its block never reads.
            self.use_if_unread(name, body);
        }
        // A tuple's items always match, so its test reads nothing; with every
        // item dropped (`is (_, _)`), nothing would read the value at all.
        if !named && matches!(pattern.kind, PatternKind::Tuple(_)) {
            self.w.line(&format!("(void){};", subject));
        }
        Ok(())
    }

    /// Generates a `match`. On a kind, it is a `switch` on the tag, with `_` as
    /// `default`; on a `maybe` or `T or E`, an `if`/`else` chain whose last case
    /// needs no test. Each case body gets its own C block, so different cases
    /// may bind the same names. The matched value is evaluated exactly once.
    fn gen_match(&mut self, expr: &Expr, cases: &[Case]) -> Result<()> {
        let ty = expr.ty()?.clone();
        let needs_temp = !matches!(expr.kind, ExprKind::Var(_));
        let subject = if needs_temp {
            let value = self.gen_expr(expr)?;
            let tmp = self.fresh_temp("match");
            self.w.open("");
            self.w.line(&format!("{} {} = {};", c_type(&ty), tmp, value));
            tmp
        } else {
            self.gen_expr(expr)?
        };

        if let Type::Named(kind_name, _) = &ty {
            if !self.kinds.contains_key(kind_name) {
                return Err(Error::internal_at(expr.span, format!("match on unknown kind '{}'", kind_name)));
            }
            // Case labels line up with their `switch`, so the switch's own braces
            // do not indent.
            self.w.line(&format!("switch ({}.tag) {{", subject));
            for case in cases {
                match &case.pattern.kind {
                    PatternKind::Variant { tag, .. } => self.w.open(&format!("case {}:", tag_value(kind_name, tag))),
                    PatternKind::Any => self.w.open("default:"),
                    _ => {
                        return Err(Error::internal_at(case.pattern.span, format!("case '{}' on kind '{}'", case.pattern.label(), kind_name)));
                    }
                }
                self.gen_pattern_bindings(&ty, &subject, &case.pattern, &case.body)?;
                self.gen_block(&case.body)?;
                self.w.line("break;");
                self.w.close();
            }
            self.w.line("}");
        } else if matches!(ty, Type::Maybe(_) | Type::Or(..)) {
            for (i, case) in cases.iter().enumerate() {
                let last = i + 1 == cases.len();
                let test = if last { None } else { Some(self.pattern_test(&ty, &subject, &case.pattern)?) };
                match (i, test) {
                    (0, Some(test)) => self.w.open(&format!("if ({})", test)),
                    // A lone `_`, which never reads the subject.
                    (0, None) => {
                        self.w.open("");
                        self.w.line(&format!("(void){};", subject));
                    }
                    (_, Some(test)) => self.w.reopen(&format!("else if ({})", test)),
                    (_, None) => self.w.reopen("else"),
                }
                self.gen_pattern_bindings(&ty, &subject, &case.pattern, &case.body)?;
                self.gen_block(&case.body)?;
            }
            if !cases.is_empty() {
                self.w.close();
            }
        } else {
            return Err(Error::internal_at(expr.span, format!("match on type {}", ty)));
        }

        if needs_temp {
            self.w.close();
        }
        Ok(())
    }

    /// An `if`/`while` condition. The statement supplies the parentheses, so a
    /// binary operation drops its own (C compilers warn about `if ((a == b))`).
    fn gen_condition(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::Binary(op, l, r) => self.gen_binary(e, *op, l, r),
            _ => self.gen_expr(e),
        }
    }

    /// Generates operands that are evaluated in order, as a call's arguments or
    /// an operator's two sides. When one of them has a prelude, they are
    /// evaluated left to right: every operand before the last prelude is stored
    /// in a temporary first, so that it is evaluated before that prelude runs
    /// (the prelude may call tasks or change variables), and so is every other
    /// operand but the last, since C evaluates the rest in no fixed order.
    fn gen_operands(&mut self, operands: &[&Expr]) -> Result<Vec<String>> {
        let last_prelude = operands.iter().rposition(|e| has_prelude(e));
        let mut values = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            let value = self.gen_expr(operand)?;
            let constant = matches!(
                operand.kind,
                ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            );
            // A variable after the last prelude reads the same value whenever C reads it.
            let variable = matches!(operand.kind, ExprKind::Var(_));
            let in_order = last_prelude.is_some_and(|last| i < last || (i + 1 < operands.len() && !variable));
            // A value-less operand (`xs.add(v)`, or a call of a task sending
            // nothing, in a list of values) runs in order as a statement.
            if in_order && *operand.ty()? == Type::Unit {
                if !value.is_empty() {
                    self.w.line(&format!("{};", value));
                }
                values.push(String::new());
            } else if in_order && !constant {
                let tmp = self.fresh_temp("arg");
                self.w.line(&format!("{} {} = {};", c_type(operand.ty()?), tmp, value));
                values.push(tmp);
            } else {
                values.push(value);
            }
        }
        Ok(values)
    }

    /// The C expression for `e`. Each kind of expression with more to do than
    /// a word has a function of its own: this function's frame is on the
    /// stack once per nesting level of an expression.
    fn gen_expr(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::IntLit(n) => Ok(n.to_string()),
            ExprKind::FloatLit(n) => Ok(c_float_literal(*n)),
            ExprKind::BoolLit(b) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            ExprKind::TextLit(s) => Ok(format!("VELA__RT__TEXT(\"{}\")", escape_c_string(s))),
            ExprKind::NoneLit => Ok("NULL".to_string()),
            ExprKind::Var(name) if self.alters && name == THIS => Ok(format!("(*{})", c_ident(name))),
            // The region the `show` being generated makes for the values written
            // by their own `str`, which has a name of its own in C (see
            // `gen_show`), so that one `show` inside another keeps its own.
            ExprKind::Var(name) if name == SHOWN => self
                .shown
                .last()
                .cloned()
                .ok_or_else(|| Error::internal_at(e.span, "the region of a 'show' read outside one")),
            ExprKind::Var(name) => Ok(c_ident(name)),
            ExprKind::Field(base, field) => self.gen_field(base, field),
            ExprKind::Index(base, index) => self.gen_index(e, base, index, None),
            ExprKind::Array(values) => self.gen_array(e, values),
            ExprKind::Tuple(values) => self.gen_tuple(e, values),
            ExprKind::Repeat { value, .. } => self.gen_repeat(e, value),
            ExprKind::Call { name, types, args } => self.gen_call(e, name, types, args),
            ExprKind::Unary(op, operand) => self.gen_unary(e, *op, operand),
            ExprKind::Binary(op, l, r) => self.gen_binary(e, *op, l, r).map(|binary| parenthesized(&binary)),
            ExprKind::Construct { kind, tag, args } => self.gen_construct(e, kind, tag, args),
            ExprKind::New { ty, value, region, .. } => self.gen_new(e, ty, value, region),
            ExprKind::Deref(inner) => self.gen_expr(inner).map(|expr| format!("(*({}))", expr)),
            ExprKind::StructInit { name, fields, .. } => self.gen_struct_init(name, fields),
            ExprKind::Range(_, _) => Err(Error::internal_at(e.span, "range outside 'each' reached code generation")),
            // The error value itself; `Wrap` makes it part of a `T or E`.
            ExprKind::Fail(error) => self.gen_expr(error),
            ExprKind::Wrap(inner) => self.gen_wrap(e, inner),
            ExprKind::PlanB { value, error, body } => self.gen_plan_b(e, value, error.as_ref(), body),
            ExprKind::Edit { items, op, args } => self.gen_edit(e.span, items, *op, args, false),
            ExprKind::Scan { args, region, .. } => self.gen_scan(e.span, args, region, false),
            ExprKind::Join { parts, region, .. } => self.gen_join(parts, region),
            ExprKind::ToText { value, region, .. } => self.gen_to_text(e, value, region),
            ExprKind::Copy { value, region, .. } => self.gen_copy(e, value, region.as_deref()),
            // The checker turns every method call into the call of the task the
            // method is (see `Checker::check_method_call`).
            ExprKind::MethodCall { name, .. } => Err(Error::internal_at(e.span, format!("the method call '{}' reached code generation", name))),
        }
    }

    /// `a + b at region`: the parts, evaluated in order, joined into new text.
    fn gen_join(&mut self, parts: &[Expr], region: &str) -> Result<String> {
        let parts: Vec<&Expr> = parts.iter().collect();
        let values = self.gen_operands(&parts)?;
        Ok(format!("vela__rt__join({}, (vela__rt__text[]){{ {} }}, {})", c_ident(region), values.join(", "), values.len()))
    }

    /// `value.str() at region`, the expression `e`, on a value with `str` built in.
    fn gen_to_text(&mut self, e: &Expr, value: &Expr, region: &str) -> Result<String> {
        let helper = match value.ty()? {
            Type::Int => "int",
            Type::Float => "float",
            Type::Bool => "bool",
            Type::Text(_) => "text",
            other => return Err(Error::internal_at(e.span, format!("'str' on a value of type {}", other))),
        };
        let value = self.gen_expr(value)?;
        Ok(format!("vela__rt__{}_str({}, {})", helper, c_ident(region), value))
    }

    /// `value.copy() at region`, the expression `e`.
    fn gen_copy(&mut self, e: &Expr, value: &Expr, region: Option<&str>) -> Result<String> {
        let region = region.ok_or_else(|| Error::internal_at(e.span, "a copy whose region isn't settled reached code generation"))?;
        let copied = self.gen_expr(value)?;
        // A value that holds nothing in a region is its own copy, as `int` is
        // for a task made from a generic one that copies its values. The region
        // is still read, so C doesn't warn about a parameter this leaves unused.
        if !self.needs_fix(&fix_type(value.ty()?)) {
            return Ok(format!("((void){}, {})", c_ident(region), parenthesized(&copied)));
        }
        Ok(format!("{}({}, {})", copy_helper("copied", value.ty()?), c_ident(region), copied))
    }

    /// `scan(prompt) at region`, at `span`, which it writes as a prelude, so it
    /// reads in order with the rest of the expression around it: the C
    /// expression for the `maybe text` it gives, or nothing when it is a
    /// `statement` of its own.
    fn gen_scan(&mut self, span: Span, args: &[Expr], region: &str, statement: bool) -> Result<String> {
        let [prompt] = args else {
            return Err(Error::internal_at(span, format!("scan with {} arguments", args.len())));
        };
        let prompt = self.gen_expr(prompt)?;
        let call = format!("vela__scan({}, {})", c_ident(region), prompt);
        if statement {
            self.w.line(&format!("{};", call));
            return Ok(String::new());
        }
        let line = self.fresh_temp("line");
        self.w.line(&format!("{} {} = {};", c_type(&Type::Maybe(Box::new(Type::Text(None)))), line, call));
        Ok(line)
    }

    /// `items.add(…)` or `items.del(…)`, at `span`, which it writes as a
    /// prelude: the C expression for the item `del` gives, or nothing for
    /// `add`, or when the change is a `statement` of its own. The slice's
    /// place is found before the arguments are evaluated, as an assignment's.
    fn gen_edit(&mut self, span: Span, items: &Expr, op: EditOp, args: &[Expr], statement: bool) -> Result<String> {
        let slice = items.ty()?.clone();
        let place = match &items.kind {
            ExprKind::Var(name) => format!("&{}", c_ident(name)),
            _ => {
                let place = self.gen_place(items)?;
                let pointer = self.fresh_temp("place");
                self.w.line(&format!("{}* {} = &{};", c_type(&slice), pointer, place));
                pointer
            }
        };
        let operands: Vec<&Expr> = args.iter().collect();
        let values = self.gen_operands(&operands)?;
        let at = self.location(span);
        let call = match (op, values.as_slice(), &slice) {
            (EditOp::Add, [value], Type::Slice(_, Some(region))) => {
                format!("{}({}, {}, {})", item_helper("add", &slice), c_ident(region), place, value)
            }
            (EditOp::Add, [index, value], Type::Slice(_, Some(region))) => {
                format!("{}({}, {}, {}, {}, {})", item_helper("insert", &slice), c_ident(region), place, index, value, at)
            }
            (EditOp::Del, [], _) => format!("{}({}, {})", item_helper("pop", &slice), place, at),
            (EditOp::Del, [index], _) => format!("{}({}, {}, {})", item_helper("del", &slice), place, index, at),
            _ => return Err(Error::internal_at(span, format!("'{}' with {} arguments on a value of type {}", op.word(), values.len(), slice))),
        };
        if statement || op == EditOp::Add {
            self.w.line(&format!("{};", call));
            return Ok(String::new());
        }
        let item = self.fresh_temp("item");
        self.w.line(&format!("{} {} = {};", c_type(item_type(&slice)), item, call));
        Ok(item)
    }

    /// `base.field`, or an array's or slice's `len`.
    fn gen_field(&mut self, base: &Expr, field: &str) -> Result<String> {
        let b = self.gen_expr(base)?;
        match base.ty()? {
            // An array's length is its type's; the array is still read.
            Type::Array(_, count) => Ok(format!("((void){}, INT64_C({}))", parenthesized(&b), count)),
            Type::Slice(..) | Type::Text(_) => Ok(format!("({}).len", b)),
            Type::Tuple(_) => {
                let i = field.parse::<usize>().map_err(|_| Error::internal_at(base.span, format!("a tuple's item '{}'", field)))?;
                Ok(format!("({}).{}", b, tuple_member(i)))
            }
            _ => Ok(format!("({}).{}", b, c_ident(field))),
        }
    }

    /// `[a, b, c]`, the expression `e`.
    fn gen_array(&mut self, e: &Expr, values: &[Expr]) -> Result<String> {
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        Ok(format!("(({}){{ .items = {{ {} }} }})", c_type(e.ty()?), given.join(", ")))
    }

    /// `(a, b)`, the expression `e`.
    fn gen_tuple(&mut self, e: &Expr, values: &[Expr]) -> Result<String> {
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        let items: Vec<String> = given.iter().enumerate().map(|(i, value)| format!(".{} = {}", tuple_member(i), value)).collect();
        Ok(format!("(({}){{ {} }})", c_type(e.ty()?), items.join(", ")))
    }

    /// `[value; count]`, the expression `e`.
    fn gen_repeat(&mut self, e: &Expr, value: &Expr) -> Result<String> {
        let value = self.gen_expr(value)?;
        Ok(format!("{}({})", item_helper("fill", e.ty()?), value))
    }

    /// A call of a task, or of `show`, the expression `e`.
    fn gen_call(&mut self, e: &Expr, name: &str, types: &[TypeArg], args: &[Expr]) -> Result<String> {
        if name == "show" {
            return self.gen_show(e, args);
        }
        if let Some(into) = number_cast(name) {
            return self.gen_number_cast(e, &into, args);
        }
        // A call of a generic task calls the one made for the types it gives.
        let given: Vec<Type> = types.iter().map(|(ty, _)| ty.clone()).collect();
        let called = instance_name(name, &given);
        // A method that changes the value it is called on is given the place it
        // is in, found before the arguments are read, as an assignment's is.
        let (first, rest) = match args.split_first() {
            Some((value, rest)) if self.altering.contains(&called) => (Some(self.gen_receiver(value)?), rest),
            _ => (None, args),
        };
        let rest: Vec<&Expr> = rest.iter().collect();
        let mut values = self.gen_operands(&rest)?;
        if let Some(first) = first {
            values.insert(0, first);
        }
        Ok(format!("{}({})", mangle(&called), values.join(", ")))
    }

    /// `show(a, b, …)`, the expression `e`: each value is written where it is
    /// read, so the values are written, and anything they do happens, left to
    /// right. Every value but the last is written without ending the line,
    /// which the last one does, so `show` of one value is the one call it
    /// always was; `show()` is the end of a line alone.
    ///
    /// A value written by its own `str` makes text, which needs a region: the
    /// line gets one of its own, freed once it ends, so the text lasts exactly
    /// as long as writing it takes (see `check::show_with_str`). Every value is
    /// written as a statement, so the free comes after the last of them.
    fn gen_show(&mut self, e: &Expr, args: &[Expr]) -> Result<String> {
        let shown = args.iter().any(|arg| expr_reads(arg, SHOWN)).then(|| self.shown_region());
        if let Some(region) = &shown {
            self.w.line(&format!("vela__rt__region {}[1] = {{ VELA__RT__REGION_EMPTY }};", c_ident(region)));
            self.shown.push(region.clone());
            // A `<-` inside one of the values leaves the task before the line
            // ends, and frees every region open then, this one with them, so it
            // counts as declared by the block until the line is written.
            if let Some(block) = self.regions.last_mut() {
                block.push(region.clone());
            }
        }
        let written = self.gen_shown(e, args);
        if let Some(region) = &shown {
            self.shown.pop();
            if let Some(block) = self.regions.last_mut() {
                block.pop();
            }
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
        written?;
        Ok(String::new())
    }

    /// The values of a `show`, each written where it is read (see `gen_show`).
    fn gen_shown(&mut self, e: &Expr, args: &[Expr]) -> Result<()> {
        let Some((last, before)) = args.split_last() else {
            self.w.line("vela__rt__show_line();");
            return Ok(());
        };
        for arg in before {
            let helper = self.shown_helper(e, arg, false)?;
            let value = self.gen_expr(arg)?;
            self.w.line(&format!("{}({});", helper, value));
        }
        let helper = self.shown_helper(e, last, true)?;
        let value = self.gen_expr(last)?;
        self.w.line(&format!("{}({});", helper, value));
        Ok(())
    }

    /// `int(x)` or `float(x)`, the expression `e`: a value turned into a number.
    /// A number turned into its own type is the value itself, a bool is 1 or 0
    /// as C makes it, and an int is a float exactly. Only a float turned into an
    /// int can fail, so only it is checked at runtime.
    fn gen_number_cast(&mut self, e: &Expr, into: &Type, args: &[Expr]) -> Result<String> {
        let [value] = args else {
            return Err(Error::internal_at(e.span, format!("'{}' called with {}", into, args.len())));
        };
        let from = value.ty()?.clone();
        let expr = self.gen_expr(value)?;
        Ok(match (into, &from) {
            (Type::Int, Type::Int) | (Type::Float, Type::Float) => expr,
            (Type::Int, Type::Bool) => format!("((int64_t){})", parenthesized(&expr)),
            (Type::Float, Type::Int) => format!("((double){})", parenthesized(&expr)),
            (Type::Int, Type::Float) => format!("vela__rt__int_of_float({}, {})", expr, self.location(e.span)),
            _ => return Err(Error::internal_at(e.span, format!("'{}' of a value of type {}", into, from))),
        })
    }

    /// The helper that writes the value `arg` given to `show`, ending the line
    /// when `ends` (see `gen_show`).
    fn shown_helper(&mut self, e: &Expr, arg: &Expr, ends: bool) -> Result<String> {
        Ok(match arg.ty()? {
            Type::Maybe(_) => maybe_helper(arg.ty()?, ends),
            Type::Int | Type::Float | Type::Bool | Type::Text(_) => written_helper(arg.ty()?, ends),
            other => return Err(Error::internal_at(e.span, format!("show called with type {}", other))),
        })
    }

    /// The address of the place a method that changes the value it is called on
    /// is called on, which the checker has made sure can change.
    fn gen_receiver(&mut self, value: &Expr) -> Result<String> {
        if let ExprKind::Var(name) = &value.kind {
            // The value a method is called on is already a pointer in a method
            // that changes it, so calling another one passes it on.
            if self.alters && name == THIS {
                return Ok(c_ident(name));
            }
            return Ok(format!("&{}", c_ident(name)));
        }
        let place = self.gen_place(value)?;
        let pointer = self.fresh_temp("place");
        self.w.line(&format!("{}* {} = &{};", c_type(value.ty()?), pointer, place));
        Ok(pointer)
    }

    /// `-operand` or `not operand`, the expression `e`.
    fn gen_unary(&mut self, e: &Expr, op: UnaryOp, operand: &Expr) -> Result<String> {
        let expr = self.gen_expr(operand)?;
        // `-x` on an int overflows only for the smallest int; a
        // negated literal can't be it.
        if op == UnaryOp::Neg && *operand.ty()? == Type::Int && int_constant(operand).is_none() {
            return Ok(format!("vela__rt__neg({}, {})", expr, self.location(e.span)));
        }
        let op_s = match op {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
        };
        Ok(format!("({}{})", op_s, expr))
    }

    /// The variant `tag` of `kind` with its payload `args`, the expression `e`.
    fn gen_construct(&mut self, e: &Expr, kind: &str, tag: &str, args: &[Expr]) -> Result<String> {
        let span = e.span;
        let variant = self
            .kinds
            .get(kind)
            .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
            .cloned()
            .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
        if args.len() != variant.payload.len() {
            return Err(Error::internal_at(
                span,
                format!("variant '{}' called with {} arguments (expected {})", tag, args.len(), variant.payload.len()),
            ));
        }
        let tag_value = tag_value(kind, tag);
        if variant.payload.is_empty() {
            return Ok(format!("(({}){{ .tag = {} }})", mangle(kind), tag_value));
        }
        let operands: Vec<&Expr> = args.iter().collect();
        let values = self.gen_operands(&operands)?;
        let fields: Vec<String> = variant
            .payload
            .iter()
            .zip(values)
            .map(|(field, value)| format!(".{} = {}", c_ident(&field.name), value))
            .collect();
        Ok(format!(
            "(({}){{ .tag = {}, .data = {{ .{} = {{ {} }} }} }})",
            mangle(kind), tag_value, c_ident(tag), fields.join(", ")
        ))
    }

    /// `new T(value) at region`, the expression `e`: a pointer, or a slice.
    fn gen_new(&mut self, e: &Expr, ty: &Type, value: &Expr, region: &str) -> Result<String> {
        let span = e.span;
        if matches!(ty, Type::Slice(..)) {
            let slice = e.ty()?;
            if matches!(&value.kind, ExprKind::Array(values) if values.is_empty()) {
                // No items, and no room until the first `add`. The region is
                // still named, so a region parameter counts as used.
                return Ok(format!("((void){}, ({}){{ .items = vela__rt__no_items(), .len = 0 }})", c_ident(region), c_type(slice)));
            }
            if let ExprKind::Repeat { value: item, count } = &value.kind {
                let [item, count] = <[String; 2]>::try_from(self.gen_operands(&[item, count])?)
                    .map_err(|_| Error::internal_at(span, "a repeated value without its count"))?;
                return Ok(format!("{}({}, {}, {}, {})", item_helper("fill", slice), c_ident(region), count, item, self.location(span)));
            }
            let source = self.gen_expr(value)?;
            let source = match value.ty()? {
                // A view of the array, which lives until the copy is made.
                Type::Array(_, count) => format!("(({}){{ .items = ({}).items, .len = INT64_C({}) }})", c_type(slice), source, count),
                _ => source,
            };
            return Ok(format!("{}({}, {})", item_helper("copy", slice), c_ident(region), source));
        }
        // Copies the value out of a one-element array compound literal,
        // which works for every type in standard C.
        let val = self.gen_expr(value)?;
        let ctype = c_type(ty);
        Ok(format!(
            "(({0}*)vela__rt__new({1}, ({0}[1]){{ {2} }}, sizeof({0}), _Alignof({0})))",
            ctype,
            c_ident(region),
            val
        ))
    }

    /// A shape literal of `name`.
    fn gen_struct_init(&mut self, name: &str, fields: &[FieldInit]) -> Result<String> {
        if fields.is_empty() {
            // Initializes the placeholder member of an empty shape.
            return Ok(format!("(({}){{ 0 }})", mangle(name)));
        }
        let operands: Vec<&Expr> = fields.iter().map(|field| &field.value).collect();
        let values = self.gen_operands(&operands)?;
        let fields: Vec<String> = fields
            .iter()
            .zip(values)
            .map(|(field, value)| format!(".{} = {}", c_ident(&field.name), value))
            .collect();
        Ok(format!("(({}){{ {} }})", mangle(name), fields.join(", ")))
    }

    /// `xs[i]` or `xs[a..b]`, the expression `e`. `place` is the array `base`
    /// names as a place, already generated, when `e` is assigned to.
    fn gen_index(&mut self, e: &Expr, base: &Expr, index: &Expr, place: Option<String>) -> Result<String> {
        if let ExprKind::Range(from, to) = &index.kind {
            return self.gen_slice(e, base, from, to);
        }
        let at = self.location(e.span);
        match base.ty()? {
            Type::Array(_, count) => {
                let constant = int_constant(index);
                let (array, index) = match place {
                    Some(array) => (array, self.gen_expr(index)?),
                    None => {
                        let [array, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                            .map_err(|_| Error::internal_at(e.span, "an index without its array"))?;
                        (array, index)
                    }
                };
                // The checker has made sure a constant index is in range.
                let index = match constant {
                    Some(constant) => constant,
                    None => format!("vela__rt__index({}, INT64_C({}), {})", index, count, at),
                };
                Ok(format!("({}).items[{}]", array, index))
            }
            Type::Slice(..) => {
                let [slice, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                    .map_err(|_| Error::internal_at(e.span, "an index without its slice"))?;
                Ok(format!("(*{}({}, {}, {}))", item_helper("at", base.ty()?), slice, index, at))
            }
            other => Err(Error::internal_at(e.span, format!("an index into a value of type {}", other))),
        }
    }

    /// `base[from..to]`, the expression `e`: a slice, or a part of text.
    fn gen_slice(&mut self, e: &Expr, base: &Expr, from: &Expr, to: &Expr) -> Result<String> {
        let at = self.location(e.span);
        if matches!(base.ty()?, Type::Text(_)) {
            let [text, from, to] = <[String; 3]>::try_from(self.gen_operands(&[base, from, to])?)
                .map_err(|_| Error::internal_at(e.span, "a part of text without two bounds"))?;
            return Ok(format!("vela__rt__text_part({}, {}, {}, {})", text, from, to, at));
        }
        let slice = e.ty()?;
        let source = match base.ty()? {
            Type::Array(_, count) => {
                // The array is in a region: the slice points at its items there.
                let array = self.gen_place(base)?;
                let items = if has_prelude(from) || has_prelude(to) {
                    let items = self.fresh_temp("items");
                    self.w.line(&format!("{}* {} = ({}).items;", c_type(item_type(slice)), items, array));
                    items
                } else {
                    format!("({}).items", array)
                };
                let [from, to] = <[String; 2]>::try_from(self.gen_operands(&[from, to])?)
                    .map_err(|_| Error::internal_at(e.span, "a slice without two bounds"))?;
                let view = format!("(({}){{ .items = {}, .len = INT64_C({}) }})", c_type(slice), items, count);
                return Ok(format!("{}({}, {}, {}, {})", item_helper("part", slice), view, from, to, at));
            }
            _ => base,
        };
        let [source, from, to] = <[String; 3]>::try_from(self.gen_operands(&[source, from, to])?)
            .map_err(|_| Error::internal_at(e.span, "a slice without two bounds"))?;
        Ok(format!("{}({}, {}, {}, {})", item_helper("part", slice), source, from, to, at))
    }

    /// A place an assignment writes to, as a C lvalue. An item of an array is
    /// part of the array's own place, which is found before the index's
    /// prelude runs, if it has one.
    fn gen_place(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::Field(base, field) if matches!(base.ty, Some(Type::Named(..))) => {
                Ok(format!("({}).{}", self.gen_place(base)?, c_ident(field)))
            }
            ExprKind::Index(base, index) if matches!(base.ty, Some(Type::Array(..))) && !matches!(index.kind, ExprKind::Range(..)) => {
                let mut array = self.gen_place(base)?;
                if has_prelude(index) {
                    let pointer = self.fresh_temp("place");
                    self.w.line(&format!("{}* {} = &{};", c_type(base.ty()?), pointer, array));
                    array = format!("*{}", pointer);
                }
                self.gen_index(e, base, index, Some(array))
            }
            _ => self.gen_expr(e),
        }
    }

    /// A value becoming part of a `maybe` or `T or E`, the type of `e`.
    fn gen_wrap(&mut self, e: &Expr, inner: &Expr) -> Result<String> {
        let target = e.ty()?;
        let ctype = c_type(target);
        match target {
            Type::Maybe(pointer) if is_pointer(pointer) => match inner.ty()? {
                Type::None => Ok("NULL".to_string()),
                _ => self.gen_expr(inner),
            },
            Type::Maybe(_) => match inner.ty()? {
                Type::None => Ok(format!("(({}){{ .has = false }})", ctype)),
                _ => Ok(format!("(({}){{ .has = true, .value = {} }})", ctype, self.gen_expr(inner)?)),
            },
            Type::Or(value, _) => {
                if let ExprKind::Fail(error) = &inner.kind {
                    return Ok(format!("(({}){{ .ok = false, .error = {} }})", ctype, self.gen_expr(error)?));
                }
                if **value == Type::Unit {
                    return Err(Error::internal_at(e.span, "a value wrapped as the success of 'unit or E'"));
                }
                Ok(format!("(({}){{ .ok = true, .value = {} }})", ctype, self.gen_expr(inner)?))
            }
            other => Err(Error::internal_at(e.span, format!("a value wrapped as {}", other))),
        }
    }

    /// A plan B, `value { body }`, the plan B `e`. Its prelude stores `value`,
    /// then runs `body` if there is nothing to extract; its C expression is the
    /// temporary holding the result (empty when it gives no value).
    fn gen_plan_b(&mut self, e: &Expr, value: &Expr, error: Option<&Binding>, body: &[Stmt]) -> Result<String> {
        let value_ty = value.ty()?.clone();
        let result_ty = e.ty()?.clone();
        let value_c = self.gen_expr(value)?;
        let subject = self.fresh_temp("subject");
        self.w.line(&format!("{} {} = {};", c_type(&value_ty), subject, value_c));
        let (present, extracted) = match &value_ty {
            Type::Maybe(inner) if is_pointer(inner) => (format!("{} != NULL", subject), subject.clone()),
            Type::Maybe(_) => (format!("{}.has", subject), format!("{}.value", subject)),
            Type::Or(..) => (format!("{}.ok", subject), format!("{}.value", subject)),
            other => return Err(Error::internal_at(e.span, format!("plan B on type {}", other))),
        };
        let result = if result_ty == Type::Unit {
            String::new()
        } else {
            let result = self.fresh_temp("value");
            self.w.line(&format!("{} {} = {};", c_type(&result_ty), result, extracted));
            result
        };
        self.w.open(&format!("if (!({}))", present));
        if let (Some((name, _)), Type::Or(_, error_ty)) = (error, &value_ty)
            && name != DISCARD
        {
            self.w.line(&format!("{} {} = {}.error;", c_type(error_ty), c_ident(name), subject));
            self.use_if_unread(name, body);
        }
        match body.split_last() {
            Some((last, rest)) if !result.is_empty() && matches!(last.kind, StmtKind::Expr(_)) => {
                // The value is stored before the block's regions are freed.
                self.enter_block();
                self.gen_statements(rest, body)?;
                if let StmtKind::Expr(fallback) = &last.kind {
                    let fallback = self.gen_expr(fallback)?;
                    self.w.line(&format!("{} = {};", result, fallback));
                }
                self.leave_block(body);
            }
            _ => self.gen_block(body)?,
        }
        self.w.close();
        Ok(result)
    }

    /// A binary operation without surrounding parentheses.
    fn gen_binary(&mut self, e: &Expr, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        if matches!(op, BinaryOp::And | BinaryOp::Or) && has_prelude(r) {
            // The right side's prelude must only run when the left side
            // doesn't decide the result.
            let left = self.gen_expr(l)?;
            let result = self.fresh_temp("cond");
            self.w.line(&format!("bool {} = {};", result, left));
            let test = if op == BinaryOp::And { result.clone() } else { format!("!{}", result) };
            self.w.open(&format!("if ({})", test));
            let right = self.gen_expr(r)?;
            self.w.line(&format!("{} = {};", result, right));
            self.w.close();
            return Ok(result);
        }
        let [left, right] = <[String; 2]>::try_from(self.gen_operands(&[l, r])?)
            .map_err(|_| Error::internal_at(e.span, "a binary operation without two operands"))?;
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
            // `m == none` tests a maybe.
            let maybe = match (l.ty()?, r.ty()?) {
                (Type::Maybe(inner), Type::None) => Some((inner, &left)),
                (Type::None, Type::Maybe(inner)) => Some((inner, &right)),
                _ => None,
            };
            if let Some((inner, value)) = maybe {
                let empty = op == BinaryOp::Eq;
                return Ok(match (is_pointer(inner), empty) {
                    (true, true) => format!("{} == NULL", value),
                    (true, false) => format!("{} != NULL", value),
                    (false, true) => format!("!{}.has", value),
                    (false, false) => format!("{}.has", value),
                });
            }
        }
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && matches!(l.ty()?, Type::Tuple(_)) {
            let not = if op == BinaryOp::Ne { "!" } else { "" };
            return Ok(format!("{}{}({}, {})", not, copy_helper("eq", l.ty()?), left, right));
        }
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && matches!(l.ty()?, Type::Text(_)) {
            // Text compares by contents, not by address.
            let not = if op == BinaryOp::Ne { "!" } else { "" };
            return Ok(format!("{}vela__rt__text_eq({}, {})", not, left, right));
        }
        if matches!(l.ty()?, Type::Text(_)) {
            // Text is ordered by its bytes.
            let symbol = match op {
                BinaryOp::Lt => "<",
                BinaryOp::Gt => ">",
                BinaryOp::Le => "<=",
                BinaryOp::Ge => ">=",
                other => return Err(Error::internal_at(e.span, format!("text operator {:?}", other))),
            };
            return Ok(format!("vela__rt__text_order({}, {}) {} 0", left, right, symbol));
        }
        if op == BinaryOp::Mod && *e.ty()? == Type::Float {
            // C's `%` is integer-only.
            return Ok(format!("vela__rt__fmod({}, {})", left, right));
        }
        // Two integer literals would be computed in C's `int`, which is often 32
        // bits: `1000000 * 1000000` must be computed in 64 bits, as Vela's `int`.
        // The checker has made sure the result fits.
        let (left, right) = match (int_constant(l), int_constant(r)) {
            (Some(left), Some(right)) if *e.ty()? == Type::Int => (left, right),
            _ => {
                if let Some(helper) = checked_int_helper(op, e, r)? {
                    return Ok(format!("vela__rt__{}({}, {}, {})", helper, left, right, self.location(e.span)));
                }
                (left, right)
            }
        };
        let op_s = match op {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        };
        Ok(format!("{} {} {}", left, op_s, right))
    }

    /// Where an operation is, for a runtime message: a C string literal
    /// `"LINE:COL in task 'NAME'"`.
    fn location(&self, span: Span) -> String {
        format!("\"{}:{} in task '{}'\"", span.line, span.col, escape_c_string(&self.task))
    }

    fn gen_range(&mut self, e: &Expr) -> Result<(String, String)> {
        match &e.kind {
            ExprKind::Range(a, b) => {
                let [av, bv] = <[String; 2]>::try_from(self.gen_operands(&[a, b])?)
                    .map_err(|_| Error::internal_at(e.span, "a range without two ends"))?;
                Ok((av, bv))
            }
            _ => Err(Error::internal_at(e.span, "each over a non-range expression")),
        }
    }
}

/// `text` in parentheses, unless it already is one parenthesized expression
/// or a plain name.
fn parenthesized(text: &str) -> String {
    let simple = text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple { text.to_string() } else { format!("({})", text) }
}

fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(..))
}

/// The member of a result struct holding result `i`.
/// The types of the `wanted` values `values` give: one each, or the several a
/// call sends, or a tuple's items.
fn given_types(wanted: usize, values: &[Expr], span: Span) -> Result<Vec<Type>> {
    if values.len() == wanted {
        return values.iter().map(|value| value.ty().cloned()).collect();
    }
    match values {
        [one] => match one.ty()? {
            Type::Results(types) | Type::Tuple(types) => Ok(types.clone()),
            other => Err(Error::internal_at(span, format!("a value of type {} received by {} names", other, wanted))),
        },
        _ => Err(Error::internal_at(span, format!("{} values received by {} names", values.len(), wanted))),
    }
}

/// The names `names` binds from the value `value` of type `ty`, each with the
/// C expression that reads it: one name takes the value, and `(a, b)` reads a
/// tuple's items.
fn bound_names<'p>(names: &'p Names, ty: &Type, value: String, bindings: &mut Vec<(&'p str, Type, String)>) {
    let Type::Tuple(items) = ty else {
        bindings.push((&names.name, ty.clone(), value));
        return;
    };
    if names.parts.is_empty() {
        bindings.push((&names.name, ty.clone(), value));
        return;
    }
    for (i, (part, item)) in names.parts.iter().zip(items).enumerate() {
        bound_names(part, item, format!("{}.{}", value, tuple_member(i)), bindings);
    }
}

/// The C member holding item `i` of a tuple: `i0`, `i1`, ….
fn tuple_member(i: usize) -> String {
    format!("i{}", i)
}

fn result_member(i: usize) -> String {
    format!("r{}", i)
}

/// The success of a `unit or E` task, as a C compound literal.
fn success(ret: &Type) -> String {
    format!("(({}){{ .ok = true }})", c_type(ret))
}

/// The runtime task that writes a value of builtin type `ty`: the one that
/// ends the line when `ends`, which the last value `show` is given does.
fn written_helper(ty: &Type, ends: bool) -> String {
    let name = match ty {
        Type::Int => "int",
        Type::Float => "float",
        Type::Bool => "bool",
        _ => "text",
    };
    format!("vela__rt__{}_{}", if ends { "show" } else { "write" }, name)
}

/// The generated task that writes a `maybe` of type `ty`, ending the line when
/// `ends` (see `written_helper`).
fn maybe_helper(ty: &Type, ends: bool) -> String {
    format!("vela__{}__{}", if ends { "show" } else { "write" }, c_type(ty).trim_start_matches("vela__"))
}

/// Whether the expression contains a plan B, a change to a slice, or a `scan`,
/// so generating it writes a prelude.
fn has_prelude(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::PlanB { .. } | ExprKind::Edit { .. } | ExprKind::Scan { .. } => true,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
        | ExprKind::Var(_) => false,
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. }
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) | ExprKind::Copy { value: inner, .. } | ExprKind::ToText { value: inner, .. } => {
            has_prelude(inner)
        }
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
            has_prelude(a) || has_prelude(b)
        }
        ExprKind::Call { args, .. }
        | ExprKind::Construct { args, .. }
        | ExprKind::Array(args)
        | ExprKind::Tuple(args)
        | ExprKind::Join { parts: args, .. } => args.iter().any(has_prelude),
        ExprKind::StructInit { fields, .. } => fields.iter().any(|field| has_prelude(&field.value)),
        // The checker rewrites a method call into a call; read as one anyway,
        // so nothing here depends on that having happened.
        ExprKind::MethodCall { value, args, .. } => has_prelude(value) || args.iter().any(has_prelude),
    }
}

/// The `maybe`, `or`, array, and slice types a program uses, which each need a
/// C struct, and the helpers it needs for them: `show` for a `maybe`, and
/// writing one before another value of the same `show`; for a
/// slice, reading an item, slicing, copying into a region, and filling one; for
/// an array, filling it. Types are kept without regions.
struct UsedTypes {
    compounds: Vec<Type>,
    /// The `maybe` types shown as the last value of a `show`, which end the line.
    shown: Vec<Type>,
    /// The `maybe` types shown before another value of the same `show`.
    written: Vec<Type>,
    slice_at: Vec<Type>,
    slice_part: Vec<Type>,
    slice_copy: Vec<Type>,
    slice_fill: Vec<Type>,
    slice_add: Vec<Type>,
    slice_insert: Vec<Type>,
    slice_del: Vec<Type>,
    slice_pop: Vec<Type>,
    array_fill: Vec<Type>,
    /// The types of the values the program copies with `copy()`.
    copied: Vec<Type>,
    /// The tuple types compared with `==` or `!=`, each after the tuples it holds.
    tuple_eq: Vec<Type>,
    /// Whether the program calls `scan`.
    scans: bool,
}

/// Adds `ty`, without regions, to `list` unless it is there.
fn note(list: &mut Vec<Type>, ty: &Type) {
    let ty = ty.without_regions();
    if !list.contains(&ty) {
        list.push(ty);
    }
}

impl UsedTypes {
    fn collect(items: &[Item]) -> UsedTypes {
        let mut used = UsedTypes {
            compounds: Vec::new(),
            shown: Vec::new(),
            written: Vec::new(),
            slice_at: Vec::new(),
            slice_part: Vec::new(),
            slice_copy: Vec::new(),
            slice_fill: Vec::new(),
            slice_add: Vec::new(),
            slice_insert: Vec::new(),
            slice_del: Vec::new(),
            slice_pop: Vec::new(),
            array_fill: Vec::new(),
            copied: Vec::new(),
            tuple_eq: Vec::new(),
            scans: false,
        };
        for item in items {
            match item {
                // The fields of a shape that takes types hold `T`, which no C
                // type answers; the shapes made from it hold the types given.
                Item::Shape(s) if s.types.is_empty() => s.fields.iter().for_each(|f| used.add(&f.ty)),
                Item::Shape(_) => {}
                Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).for_each(|f| used.add(&f.ty)),
                // A generic task's own types hold `T`, which no C type
                // answers; the tasks made from it hold the types it is given.
                Item::Task(t) if t.types.is_empty() => {
                    t.params.iter().for_each(|p| used.add(&p.ty));
                    used.add(&t.ret);
                    used.block(&t.body);
                }
                Item::Task(_) => {}
            }
        }
        used
    }

    /// Records `ty` and every type inside it. Regions don't change a type's C
    /// struct, so they are left out.
    fn add(&mut self, ty: &Type) {
        let ty = &ty.without_regions();
        match ty {
            Type::Ptr(inner, _) | Type::Range(inner) | Type::Fail(inner) => self.add(inner),
            Type::Maybe(inner) => {
                self.add(inner);
                if !is_pointer(inner) && !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            Type::Or(value, error) => {
                self.add(value);
                self.add(error);
                if !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            Type::Results(types) | Type::Tuple(types) => {
                types.iter().for_each(|item| self.add(item));
                if !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            Type::Array(inner, _) | Type::Slice(inner, _) => {
                self.add(inner);
                if !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            _ => {}
        }
    }

    /// Records that tuples of type `tuple` are compared, and so are the tuples
    /// in it, which come first.
    fn note_tuple_eq(&mut self, tuple: &Type) {
        let Type::Tuple(items) = tuple.without_regions() else { return };
        items.iter().for_each(|item| self.note_tuple_eq(item));
        note(&mut self.tuple_eq, tuple);
    }

    /// Records the types of the names a binding declares, and of the tuples it unpacks.
    fn bound(&mut self, bound: &Bound) {
        if let Some(ty) = &bound.ty {
            self.add(ty);
        }
        bound.parts.iter().for_each(|part| self.bound(part));
    }

    /// Records what the places of an assignment read.
    fn target(&mut self, target: &Target) {
        match target {
            Target::Discard(_) => {}
            Target::Place(place) => self.expr(place),
            Target::Tuple(parts, _) => parts.iter().for_each(|part| self.target(part)),
        }
    }

    fn block(&mut self, block: &[Stmt]) {
        for stmt in block {
            match &stmt.kind {
                StmtKind::Expr(e) => self.expr(e),
                StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().for_each(|value| self.expr(value)),
                StmtKind::Region(_) => {}
                StmtKind::Binding { names, init, .. } => {
                    names.iter().for_each(|bound| self.bound(bound));
                    init.iter().for_each(|value| self.expr(value));
                }
                StmtKind::Assign(targets, values) => {
                    targets.iter().for_each(|target| self.target(target));
                    values.iter().for_each(|value| self.expr(value));
                }
                StmtKind::When(cond, then_b, else_b) => {
                    self.expr(&cond.value);
                    self.block(then_b);
                    if let Some(else_b) = else_b {
                        self.block(else_b);
                    }
                }
                StmtKind::Loop(cond, body) => {
                    self.expr(&cond.value);
                    self.block(body);
                }
                StmtKind::Each(_, range, body) => {
                    self.expr(range);
                    self.block(body);
                }
                StmtKind::Match(subject, cases) => {
                    self.expr(subject);
                    cases.iter().for_each(|case| self.block(&case.body));
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        if let Some(ty) = &e.ty {
            self.add(ty);
        }
        match &e.kind {
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            | ExprKind::Var(_) => {}
            ExprKind::New { ty, value, .. } if matches!(**ty, Type::Slice(..)) => {
                let Some(slice) = &e.ty else { return };
                match &value.kind {
                    // The repeated value fills the slice; it makes no array.
                    ExprKind::Repeat { value: item, count } => {
                        note(&mut self.slice_fill, slice);
                        self.expr(item);
                        self.expr(count);
                    }
                    // `new [T]()` copies nothing.
                    ExprKind::Array(values) if values.is_empty() => {}
                    _ => {
                        note(&mut self.slice_copy, slice);
                        self.expr(value);
                    }
                }
            }
            ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. }
            | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => self.expr(inner),
            ExprKind::Index(base, index) => {
                match (&index.kind, &base.ty, &e.ty) {
                    // A part of text needs no helper of its own.
                    (ExprKind::Range(..), _, Some(slice @ Type::Slice(..))) => note(&mut self.slice_part, slice),
                    (_, Some(slice @ Type::Slice(..)), _) => note(&mut self.slice_at, slice),
                    _ => {}
                }
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Repeat { value, count } => {
                if let Some(array) = &e.ty {
                    note(&mut self.array_fill, array);
                }
                self.expr(value);
                self.expr(count);
            }
            ExprKind::Array(values) | ExprKind::Tuple(values) => values.iter().for_each(|value| self.expr(value)),
            ExprKind::Binary(op, a, b) => {
                if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
                    && let Some(tuple @ Type::Tuple(_)) = &a.ty
                {
                    self.note_tuple_eq(tuple);
                }
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Range(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Call { name, args, .. } => {
                if name == "show" {
                    for (i, arg) in args.iter().enumerate() {
                        // Its text may name a region, which C doesn't see. The
                        // last value ends the line and the others don't, so
                        // each needs the helper for its place.
                        if let Some(ty @ Type::Maybe(_)) = &arg.ty {
                            let ends = i + 1 == args.len();
                            note(if ends { &mut self.shown } else { &mut self.written }, ty);
                        }
                    }
                }
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::MethodCall { value, args, .. } => {
                self.expr(value);
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::Construct { args, .. } => args.iter().for_each(|arg| self.expr(arg)),
            ExprKind::StructInit { fields, .. } => fields.iter().for_each(|field| self.expr(&field.value)),
            ExprKind::PlanB { value, body, .. } => {
                self.expr(value);
                self.block(body);
            }
            ExprKind::Copy { value, .. } => {
                if let Some(ty) = &value.ty {
                    note(&mut self.copied, ty);
                }
                self.expr(value);
            }
            ExprKind::Join { parts, .. } => parts.iter().for_each(|part| self.expr(part)),
            ExprKind::ToText { value, .. } => self.expr(value),
            ExprKind::Scan { args, .. } => {
                self.scans = true;
                // Its `maybe text` has a struct even when it isn't kept.
                self.add(&Type::Maybe(Box::new(Type::Text(None))));
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::Edit { items, op, args } => {
                if let Some(slice @ Type::Slice(..)) = &items.ty {
                    let list = match (op, args.len()) {
                        (EditOp::Add, 1) => &mut self.slice_add,
                        (EditOp::Add, _) => &mut self.slice_insert,
                        (EditOp::Del, 0) => &mut self.slice_pop,
                        (EditOp::Del, _) => &mut self.slice_del,
                    };
                    note(list, slice);
                }
                self.expr(items);
                args.iter().for_each(|arg| self.expr(arg));
            }
        }
    }
}

/// Whether any statement in `block` reads the variable `name`, as C's
/// unused-variable warnings see it: assigning to the variable or to one of its
/// fields is not a read. (Vela has no shadowing, so every `name` in the
/// variable's scope refers to it.)
fn block_reads(block: &[Stmt], name: &str) -> bool {
    block.iter().any(|stmt| stmt_reads(stmt, name))
}

fn stmt_reads(stmt: &Stmt, name: &str) -> bool {
    let expr = |e: &Expr| expr_reads(e, name);
    let body = |b: &Block| block_reads(b, name);
    match &stmt.kind {
        StmtKind::Expr(e) => expr(e),
        StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().any(expr),
        StmtKind::Region(_) => false,
        StmtKind::Binding { init, .. } => init.iter().any(expr),
        StmtKind::Assign(targets, values) => {
            targets.iter().any(|target| target_reads(target, name)) || values.iter().any(expr)
        }
        StmtKind::When(cond, then_b, else_b) => expr(&cond.value) || body(then_b) || else_b.as_ref().is_some_and(body),
        StmtKind::Loop(cond, b) => expr(&cond.value) || body(b),
        StmtKind::Each(_, range, b) => expr(range) || body(b),
        StmtKind::Match(subject, cases) => expr(subject) || cases.iter().any(|case| body(&case.body)),
    }
}

/// Whether assigning to `target` reads `name`. Writing to a variable or its
/// fields doesn't read it; writing through a pointer (`p^ = …`) reads `p`.
fn target_reads(target: &Target, name: &str) -> bool {
    match target {
        Target::Discard(_) => false,
        Target::Place(place) => place_reads(place, name),
        Target::Tuple(parts, _) => parts.iter().any(|part| target_reads(part, name)),
    }
}

/// Whether assigning to the place `target` reads `name`.
fn place_reads(target: &Expr, name: &str) -> bool {
    match &target.kind {
        ExprKind::Var(_) => false,
        ExprKind::Field(base, _) => place_reads(base, name),
        // Writing an item of an array writes the array; a slice is read to
        // find its item.
        ExprKind::Index(base, index) if matches!(base.ty, Some(Type::Array(..))) => place_reads(base, name) || expr_reads(index, name),
        _ => expr_reads(target, name),
    }
}

fn expr_reads(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Var(var) => var == name,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit => false,
        // `new` reads the region it allocates in.
        ExprKind::New { value, region, .. } => region == name || expr_reads(value, name),
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner)
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => expr_reads(inner, name),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
            expr_reads(a, name) || expr_reads(b, name)
        }
        ExprKind::Call { args, .. } | ExprKind::Construct { args, .. } | ExprKind::Array(args) | ExprKind::Tuple(args) => {
            args.iter().any(|arg| expr_reads(arg, name))
        }
        ExprKind::MethodCall { value, args, .. } => expr_reads(value, name) || args.iter().any(|arg| expr_reads(arg, name)),
        ExprKind::StructInit { fields, .. } => fields.iter().any(|field| expr_reads(&field.value, name)),
        ExprKind::PlanB { value, body, .. } => expr_reads(value, name) || block_reads(body, name),
        // `scan` reads the region it keeps the line in.
        ExprKind::Scan { args, region, .. } => region == name || args.iter().any(|arg| expr_reads(arg, name)),
        // Joining text, and a value turned into text, read the region the text goes in.
        ExprKind::Join { parts, region, .. } => region == name || parts.iter().any(|part| expr_reads(part, name)),
        ExprKind::ToText { value, region, .. } => region == name || expr_reads(value, name),
        // `copy` reads the region it copies into.
        ExprKind::Copy { value, region, .. } => region.as_deref() == Some(name) || expr_reads(value, name),
        // `add` reads the region it may allocate in.
        ExprKind::Edit { items, op, args } => {
            let region = match &items.ty {
                Some(Type::Slice(_, Some(region))) if *op == EditOp::Add => region == name,
                _ => false,
            };
            region || expr_reads(items, name) || args.iter().any(|arg| expr_reads(arg, name))
        }
    }
}

fn type_name(item: &Item) -> Option<&str> {
    match item {
        // A shape that takes types is not a C type: see `generate`.
        Item::Shape(s) if !s.types.is_empty() => None,
        Item::Shape(s) => Some(&s.name),
        Item::Kind(k) => Some(&k.name),
        Item::Task(_) => None,
    }
}

/// A type with a C struct definition: a shape or kind, or a `maybe` or `or`.
enum TypeDef<'a> {
    Item(&'a Item),
    Compound(&'a Type),
}

/// The C names of the struct definitions a value of type `ty` contains by
/// value, which must come before any struct embedding it.
fn value_dependencies(ty: &Type) -> Vec<String> {
    match ty {
        Type::Named(name, _) => vec![mangle(name)],
        Type::Maybe(inner) if is_pointer(inner) => Vec::new(),
        Type::Maybe(_) | Type::Or(..) | Type::Results(_) | Type::Tuple(_) | Type::Array(..) | Type::Slice(..) => vec![c_type(ty)],
        _ => Vec::new(),
    }
}

/// Shapes, kinds, and compound types in source order, except that each comes
/// after every type it contains by value, since C needs a struct's complete
/// definition before it can be embedded. (The checker has rejected types that
/// contain themselves by value.)
fn type_order<'a>(items: &'a [Item], compounds: &'a [Type]) -> Result<Vec<TypeDef<'a>>> {
    let mut by_name: HashMap<String, TypeDef<'a>> = HashMap::new();
    let mut names = Vec::new();
    for item in items {
        if let Some(name) = type_name(item) {
            names.push(mangle(name));
            by_name.insert(mangle(name), TypeDef::Item(item));
        }
    }
    for ty in compounds {
        names.push(c_type(ty));
        by_name.insert(c_type(ty), TypeDef::Compound(ty));
    }
    let mut finished = HashMap::new();
    let mut order = Vec::new();
    for name in &names {
        visit_type(name, &mut by_name, &mut finished, &mut order)?;
    }
    Ok(order)
}

/// Depth-first step of `type_order`, from `root`. `finished` maps a type to
/// `false` while its dependencies are being emitted and `true` once it is in
/// `order`. The search keeps its own stack, so a long chain of types can't
/// overflow the compiler's.
fn visit_type<'a>(
    root: &str,
    by_name: &mut HashMap<String, TypeDef<'a>>,
    finished: &mut HashMap<String, bool>,
    order: &mut Vec<TypeDef<'a>>,
) -> Result<()> {
    // Each type being entered, with its dependencies, reversed so the next is last.
    let mut stack: Vec<(String, TypeDef<'a>, Vec<String>)> = Vec::new();
    let mut next = Some(root.to_string());
    loop {
        if let Some(name) = next.take() {
            match finished.get(&name) {
                Some(true) => {}
                Some(false) => return Err(Error::internal(format!("type '{}' contains itself by value", name))),
                // A builtin type has no definition.
                None => {
                    if let Some(def) = by_name.remove(&name) {
                        finished.insert(name.clone(), false);
                        let mut dependencies = dependencies_of(&def);
                        dependencies.reverse();
                        stack.push((name, def, dependencies));
                    }
                }
            }
        }
        let Some((_, _, dependencies)) = stack.last_mut() else { return Ok(()) };
        match dependencies.pop() {
            Some(dependency) => next = Some(dependency),
            None => {
                if let Some((name, def, _)) = stack.pop() {
                    finished.insert(name, true);
                    order.push(def);
                }
            }
        }
    }
}

/// The types a definition contains by value.
fn dependencies_of(def: &TypeDef) -> Vec<String> {
    match def {
        TypeDef::Item(Item::Shape(s)) => s.fields.iter().flat_map(|f| value_dependencies(&f.ty)).collect(),
        TypeDef::Item(Item::Kind(k)) => k.variants.iter().flat_map(|v| &v.payload).flat_map(|f| value_dependencies(&f.ty)).collect(),
        TypeDef::Item(Item::Task(_)) => Vec::new(),
        TypeDef::Compound(Type::Maybe(inner)) => value_dependencies(inner),
        TypeDef::Compound(Type::Or(value, error)) => value_dependencies(value).into_iter().chain(value_dependencies(error)).collect(),
        TypeDef::Compound(Type::Results(types) | Type::Tuple(types)) => types.iter().flat_map(value_dependencies).collect(),
        TypeDef::Compound(Type::Array(items, _)) => value_dependencies(items),
        TypeDef::Compound(_) => Vec::new(),
    }
}

/// The C declarator of a task, e.g. `int64_t vela_add(int64_t a, int64_t b)`.
fn signature(t: &TaskDef, alters: bool) -> String {
    let params: Vec<String> = t
        .params
        .iter()
        .map(|p| {
            // A method that changes the value it is called on is given the
            // place it is in, so what it writes is what the caller holds.
            if alters && p.name == THIS {
                return format!("{}* {}", c_type(&p.ty), c_ident(&p.name));
            }
            format!("{} {}", c_type(&p.ty), c_ident(&p.name))
        })
        .collect();
    // `()` in C declares a function with unspecified parameters; `(void)` means none.
    let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
    format!("{} {}({})", c_type(&t.ret), mangle(&t.name), params)
}

// C names. Vela names start with an ASCII letter and never contain `__`, so
// the generated C names below have distinct shapes and can't collide with each
// other, with the runtime (`vela__rt__*`), or with C:
//
//   vela_Point, vela_area         tasks, shapes, kinds          (`mangle`)
//   vela_shapes__1__geometry__0__area   another module's          (`c_name`)
//   vela___2____1__common__1__text__0__pad   one above the main file's folder
//   vela_Shape__Tag               a kind's tag enum type        (`tag_type`)
//   vela_Shape__tag_Circle        a variant's tag value         (`tag_value`)
//   vela__int                     a local or field named like C (`c_ident`)
//   vela__maybe__int              a `maybe` struct              (`c_type`)
//   vela__or__int__n_ParseError   an `or` struct                (`c_type`)
//   vela__show__maybe__int        `show` for a `maybe`          (`maybe_helper`)
//   vela__write__maybe__int       one written before another    (`maybe_helper`)
//   vela__array__4__int           an array struct               (`c_type`)
//   vela__slice__int              a slice struct                (`c_type`)
//   vela__at__slice__int          a helper for a slice or array (`item_helper`)
//   vela__end__0, vela__empty__   compiler temporaries and members
//
// A region is a local or parameter like any other (`c_ident`); regions don't
// change C types, so `^int@a` and `^int@b` are both `int64_t*`.

/// The C name of a task, shape, or kind.
fn mangle(name: &str) -> String {
    format!("vela_{}", c_name(name))
}

fn tag_type(kind: &str) -> String {
    format!("vela_{}__Tag", c_name(kind))
}

fn tag_value(kind: &str, tag: &str) -> String {
    format!("vela_{}__tag_{}", c_name(kind), tag)
}

/// A task, shape, or kind name as part of a C name: the name with its module's
/// path folded into it (see `flat_name`). No Vela name contains `__`, and none
/// starts with a digit, so the result can't be any other name, or read as a
/// word the generated names use.
fn c_name(name: &str) -> String {
    flat_name(name)
}

/// The C name of a local, parameter, field, or variant union member: the Vela
/// name itself, unless C could read it as something else, such as a keyword
/// or a macro from a standard header.
fn c_ident(name: &str) -> String {
    // The value a method is called on is named `.this`, which no Vela name can
    // be (see `ast::THIS`), so C needs a name for it.
    if name == THIS {
        return "vela__this".to_string();
    }
    if collides_with_c(name) {
        format!("vela__{}", name)
    } else {
        name.to_string()
    }
}

fn collides_with_c(name: &str) -> bool {
    C_RESERVED.contains(&name)
        // Our own prefix, and POSIX's reserved `_t` type suffix.
        || name.starts_with("vela_")
        || name.ends_with("_t")
        // Object-like macros such as EOF, NULL, and INT64_MAX.
        || name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// C11 and C23 keywords, and lowercase object-like macros that the runtime's
/// headers (or common compilers) may define.
const C_RESERVED: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "char", "const", "constexpr",
    "continue", "default", "do", "double", "else", "enum", "extern", "false", "float", "for",
    "goto", "if", "inline", "int", "long", "nullptr", "register", "restrict", "return", "short",
    "signed", "sizeof", "static", "static_assert", "struct", "switch", "thread_local", "true",
    "typedef", "typeof", "typeof_unqual", "union", "unsigned", "void", "volatile", "while",
    "asm", "complex", "errno", "imaginary", "linux", "math_errhandling", "noreturn", "stderr",
    "stdin", "stdout", "unix", "L_tmpnam",
];

fn c_type(ty: &Type) -> String {
    match ty {
        // A generic declaration is never generated: each use is generated with
        // the type it gives, so these would be compiler bugs (see `instances`
        // and `shape_instance`).
        Type::Param(name) => format!("/*type {}*/", name),
        Type::Applied(applied) => format!("/*shape {}*/", applied.name),
        Type::Int => "int64_t".to_string(),
        Type::Float => "double".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text(_) => "vela__rt__text".to_string(),
        Type::Unit => "void".to_string(),
        Type::None => "void*".to_string(),
        Type::Ptr(t, _) => format!("{}*", c_type(t)),
        Type::Named(n, _) => mangle(n),
        Type::Region => "vela__rt__region*".to_string(),
        Type::Range(t) => format!("/*range<{}>*/", c_type(t)),
        // A `maybe` pointer is the pointer itself, with `NULL` for none.
        Type::Maybe(inner) if is_pointer(inner) => c_type(inner),
        Type::Maybe(inner) => format!("vela__maybe__{}", type_word(inner)),
        Type::Or(value, error) => format!("vela__or__{}__{}", type_word(value), type_word(error)),
        Type::Fail(error) => c_type(error),
        Type::Results(_) | Type::Tuple(_) | Type::Array(..) | Type::Slice(..) => format!("vela__{}", type_word(ty)),
    }
}

/// The type of the items of an array or slice type.
fn item_type(ty: &Type) -> &Type {
    match ty {
        Type::Array(items, _) | Type::Slice(items, _) => items,
        other => other,
    }
}

/// The C name of a generated helper for an array or slice type:
/// `vela__at__slice__int`, `vela__fill__array__4__int`.
fn item_helper(what: &str, ty: &Type) -> String {
    format!("vela__{}__{}", what, type_word(ty))
}

/// The C name of a generated helper for copies of values of type `ty`:
/// `vela__copied__ptr__n_Node`, `vela__fix__text`.
fn copy_helper(what: &str, ty: &Type) -> String {
    format!("vela__{}__{}", what, type_word(&ty.without_regions()))
}

/// The type whose fix a value of type `ty` takes: a `maybe` pointer is the
/// pointer itself in C, `NULL` for none.
fn fix_type(ty: &Type) -> Type {
    match ty.without_regions() {
        Type::Maybe(inner) if is_pointer(&inner) => *inner,
        other => other,
    }
}

/// Formats a float so C always reads it as a `double` literal: `3.0` rather
/// than `3` (which C would treat as an integer), and exponent form such as
/// `1e300` for very large or small magnitudes. The lexer rejects non-finite
/// literals, so `inf`/`NaN` never reach here.
fn c_float_literal(n: f64) -> String {
    // Debug formatting always includes a decimal point or an exponent.
    format!("{:?}", n)
}

/// Escapes text for a C string literal. Everything outside printable ASCII
/// (control characters, NUL, and each byte of a non-ASCII character) becomes a
/// three-digit octal escape, which, unlike `\x`, can't absorb the characters
/// after it; `?` is escaped so C never sees a trigraph such as `??!`.
fn escape_c_string(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'?' => out.push_str("\\?"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b' '..=b'~' => out.push(char::from(byte)),
            _ => out.push_str(&format!("\\{:03o}", byte)),
        }
    }
    out
}

/// The runtime helper that computes the int operation `e` (`op` on the
/// operands, `r` the right one), stopping the program when its result doesn't
/// fit or it divides by zero; `None` when C's operator can't go wrong: `e`
/// isn't int arithmetic, or it divides by a literal other than 0 and -1.
fn checked_int_helper(op: BinaryOp, e: &Expr, r: &Expr) -> Result<Option<&'static str>> {
    if *e.ty()? != Type::Int {
        return Ok(None);
    }
    let safe_divisor = int_value(r).is_some_and(|divisor| divisor != 0 && divisor != -1);
    Ok(match op {
        BinaryOp::Add => Some("add"),
        BinaryOp::Sub => Some("sub"),
        BinaryOp::Mul => Some("mul"),
        BinaryOp::Div if !safe_divisor => Some("div"),
        BinaryOp::Mod if !safe_divisor => Some("mod"),
        _ => None,
    })
}

/// The value of an integer literal, possibly negated.
fn int_value(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Unary(UnaryOp::Neg, operand) => int_value(operand).and_then(i64::checked_neg),
        _ => None,
    }
}

/// An integer literal, possibly negated, as a 64-bit C constant: `INT64_C(5)`,
/// `(-INT64_C(5))`. `None` for any other expression.
fn int_constant(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(format!("INT64_C({})", n)),
        ExprKind::Unary(UnaryOp::Neg, operand) => int_constant(operand).map(|inner| format!("(-{})", inner)),
        _ => None,
    }
}
