use proc_macro::TokenStream;
use std::path::PathBuf;
use syn::{LitStr, parse_macro_input};
use wit_parser::{
    Docs, Function, FunctionKind, Handle, InterfaceId, Resolve, Span, Type, TypeDefKind, TypeId,
    WorldId, WorldItem, WorldKey,
};

/// Doc-comment annotations start with this prefix, e.g.
/// `/// @dist_gen(idempotent, timeout = 10)`.
const ANNOTATION_PREFIX: &str = "@dist_gen";

/// Parses a WIT file (or directory of WIT files) at compile time and prints
/// its packages, interfaces, types, functions and worlds with their
/// annotations.
///
/// The path is resolved relative to the `CARGO_MANIFEST_DIR` of the crate
/// invoking the macro. Parse errors and malformed annotations are reported
/// as compile errors.
///
/// ```ignore
/// read_wit!("wit/world.wit");
/// ```
#[proc_macro]
pub fn read_wit(input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(input as LitStr);

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let path = PathBuf::from(manifest_dir).join(lit.value());

    let mut resolve = Resolve::default();
    let sources = match resolve.push_path(&path) {
        Ok((_, sources)) => sources,
        Err(e) => {
            return syn::Error::new(lit.span(), format!("failed to parse WIT: {e:?}"))
                .to_compile_error()
                .into();
        }
    };

    // `include_str!` makes cargo track each WIT file, so the macro reruns
    // whenever one of them changes. This is emitted even when there are
    // annotation errors, so fixing the WIT file triggers a rebuild.
    let mut output: TokenStream = sources
        .paths()
        .map(|p| format!("const _: &str = include_str!({:?});\n", p.display().to_string()))
        .collect::<String>()
        .parse()
        .unwrap();

    let mut printer = Printer {
        resolve: &resolve,
        errors: Vec::new(),
    };
    printer.print_resolve();

    for error in &printer.errors {
        // Pointing at the path literal; the message holds the WIT location.
        let error = syn::Error::new(lit.span(), error).to_compile_error();
        output.extend(TokenStream::from(error));
    }
    output
}

struct Printer<'a> {
    resolve: &'a Resolve,
    /// Malformed annotations, reported as compile errors once printing is done.
    errors: Vec<String>,
}

impl Printer<'_> {
    fn print_resolve(&mut self) {
        let resolve = self.resolve;
        for (_, package) in &resolve.packages {
            let what = format!("package `{}`", package.name);
            let ann = self.annotations(&package.docs, None, &what);
            eprintln!("package {}{ann}", package.name);

            eprintln!("    interfaces:");
            for (name, &id) in &package.interfaces {
                let ann = format_annotations(&self.interface_annotations(id));
                eprintln!("        {name}{ann}");
                self.print_interface_body(id, "            ");
            }

            eprintln!("    worlds:");
            for &world_id in package.worlds.values() {
                self.print_world(world_id);
            }
        }
    }

    fn print_world(&mut self, id: WorldId) {
        let world = &self.resolve.worlds[id];
        let what = format!("world `{}`", world.name);
        let ann = self.annotations(&world.docs, Some(world.span), &what);
        eprintln!("        {}{ann}", world.name);
        self.print_world_items("import", world.imports.iter());
        self.print_world_items("export", world.exports.iter());
    }

    fn print_world_items<'b>(
        &mut self,
        direction: &str,
        items: impl Iterator<Item = (&'b WorldKey, &'b WorldItem)>,
    ) {
        let resolve = self.resolve;
        for (key, item) in items {
            match item {
                WorldItem::Interface { id, docs, span, .. } => {
                    let name = resolve.name_world_key(key);
                    // Inline `import foo: interface { ... }` declarations have no name.
                    let kind = match resolve.interfaces[*id].name {
                        Some(_) => "named",
                        None => "anonymous",
                    };
                    // Annotations can sit on the `import`/`export` statement
                    // or on the interface definition itself.
                    let what = format!("{direction} `{name}`");
                    let mut found = self.parse(docs, Some(*span), &what);
                    found.extend(self.interface_annotations(*id));
                    let ann = format_annotations(&found);
                    eprintln!("            {direction} interface {name} ({kind}){ann}");
                    self.print_interface_body(*id, "                ");
                }
                // Functions declared directly in the world, outside any interface.
                WorldItem::Function(func) => {
                    eprint!("            {direction} ");
                    self.print_function(func, "");
                }
                // Types brought into the world with `use`, or declared in it.
                WorldItem::Type { id, .. } => {
                    eprint!("            {direction} ");
                    self.print_type(*id, "", &[]);
                }
            }
        }
    }

    /// Annotations on an interface definition.
    fn interface_annotations(&mut self, id: InterfaceId) -> Vec<String> {
        let interface = &self.resolve.interfaces[id];
        let what = match &interface.name {
            Some(name) => format!("interface `{name}`"),
            None => "anonymous interface".to_string(),
        };
        self.parse(&interface.docs, Some(interface.span), &what)
    }

    /// Prints the types and functions of an interface. Resource methods are
    /// printed under their resource instead of as loose functions.
    fn print_interface_body(&mut self, id: InterfaceId, indent: &str) {
        let interface = &self.resolve.interfaces[id];
        let functions: Vec<&Function> = interface.functions.values().collect();

        for &type_id in interface.types.values() {
            self.print_type(type_id, indent, &functions);
        }
        for func in functions.iter().filter(|f| f.kind.resource().is_none()) {
            self.print_function(func, indent);
        }
    }

    /// Prints a type definition along with its fields/cases/flags and, for
    /// resources, the methods among `functions` that belong to it.
    fn print_type(&mut self, id: TypeId, indent: &str, functions: &[&Function]) {
        let resolve = self.resolve;
        let def = &resolve.types[id];
        let name = def.name.as_deref().unwrap_or("<unnamed>");
        let ann = self.annotations(&def.docs, Some(def.span), &format!("type `{name}`"));

        // A `use`d type is an alias to the type it refers to.
        let kind = match &def.kind {
            TypeDefKind::Type(t) => format!("alias of {}", type_name(resolve, t)),
            kind => kind.as_str().to_string(),
        };
        eprintln!("{indent}type {name} ({kind}){ann}");

        // Members have no span of their own, so errors point at the type.
        let inner = format!("{indent}    ");
        let mut member = |label: &str, member: &str, docs: &Docs| {
            let what = format!("{label} `{name}.{member}`");
            let ann = self.annotations(docs, Some(def.span), &what);
            eprintln!("{inner}{label} {member}{ann}");
        };
        match &def.kind {
            TypeDefKind::Record(r) => {
                for f in &r.fields {
                    let field = format!("{}: {}", f.name, type_name(resolve, &f.ty));
                    member("field", &field, &f.docs);
                }
            }
            TypeDefKind::Variant(v) => {
                for c in &v.cases {
                    let case = match &c.ty {
                        Some(ty) => format!("{}({})", c.name, type_name(resolve, ty)),
                        None => c.name.clone(),
                    };
                    member("case", &case, &c.docs);
                }
            }
            TypeDefKind::Enum(e) => {
                for c in &e.cases {
                    member("case", &c.name, &c.docs);
                }
            }
            TypeDefKind::Flags(f) => {
                for flag in &f.flags {
                    member("flag", &flag.name, &flag.docs);
                }
            }
            TypeDefKind::Resource => {
                for func in functions.iter().filter(|f| f.kind.resource() == Some(id)) {
                    self.print_function(func, &inner);
                }
            }
            _ => {}
        }
    }

    fn print_function(&mut self, func: &Function, indent: &str) {
        let resolve = self.resolve;
        let mode = if func.kind.is_async() { "async" } else { "sync" };
        let role = match func.kind {
            FunctionKind::Freestanding | FunctionKind::AsyncFreestanding => "function",
            FunctionKind::Constructor(_) => "constructor",
            FunctionKind::Static(_) | FunctionKind::AsyncStatic(_) => "static",
            _ => "method",
        };
        let params = func
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, type_name(resolve, &p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let result = func
            .result
            .as_ref()
            .map_or("-".to_string(), |ty| type_name(resolve, ty));
        let what = format!("function `{}`", func.name);
        let ann = self.annotations(&func.docs, Some(func.span), &what);
        eprintln!(
            "{indent}{role} {}  kind={mode}  params=({params})  result={result}{ann}",
            func.item_name()
        );
    }

    /// Parses the annotations of `docs` and formats them for printing.
    fn annotations(&mut self, docs: &Docs, span: Option<Span>, what: &str) -> String {
        format_annotations(&self.parse(docs, span, what))
    }

    /// Parses the annotations of `docs`, recording an error on failure.
    /// `what` and `span` describe the documented item for the error message.
    fn parse(&mut self, docs: &Docs, span: Option<Span>, what: &str) -> Vec<String> {
        let Some(text) = &docs.contents else {
            return Vec::new();
        };
        parse_annotations(text).unwrap_or_else(|e| {
            let location =
                span.map_or(String::new(), |s| format!("{}: ", self.resolve.render_location(s)));
            let error = format!("{location}{e} in doc comment of {what}");
            // Interfaces are printed both at the root and in worlds.
            if !self.errors.contains(&error) {
                self.errors.push(error);
            }
            Vec::new()
        })
    }
}

fn format_annotations(annotations: &[String]) -> String {
    if annotations.is_empty() {
        String::new()
    } else {
        format!("  => {}", annotations.join(", "))
    }
}

/// Finds every `<ANNOTATION_PREFIX>(...)` in a doc comment, returning each
/// whole annotation. Nested parentheses and quoted strings are respected.
/// Unbalanced parentheses and unterminated strings are an error.
fn parse_annotations(text: &str) -> Result<Vec<String>, String> {
    let opener = format!("{ANNOTATION_PREFIX}(");
    let mut found = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find(&opener) {
        let body = &rest[start + opener.len()..];
        let mut depth = 1;
        let mut in_string = false;
        let mut escaped = false;
        let mut end = None;

        for (i, c) in body.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '(' if !in_string => depth += 1,
                ')' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }

        let Some(end) = end else {
            let problem = if in_string {
                "unterminated string"
            } else {
                "unbalanced parentheses (missing `)`)"
            };
            let line = rest[start..].lines().next().unwrap_or_default();
            return Err(format!("{problem} in `{line}`"));
        };
        let annotation = format!("{opener}{})", &body[..end]);
        rest = &body[end + 1..];

        // `@dist_gen(a))`: the annotation closed, but another `)` follows.
        if rest.starts_with(')') {
            return Err(format!(
                "unbalanced parentheses (extra `)`) after `{annotation}`"
            ));
        }
        found.push(annotation);
    }
    Ok(found)
}

/// Renders a type the way it would be written in WIT.
fn type_name(resolve: &Resolve, ty: &Type) -> String {
    let id = match ty {
        Type::Bool => return "bool".into(),
        Type::U8 => return "u8".into(),
        Type::U16 => return "u16".into(),
        Type::U32 => return "u32".into(),
        Type::U64 => return "u64".into(),
        Type::S8 => return "s8".into(),
        Type::S16 => return "s16".into(),
        Type::S32 => return "s32".into(),
        Type::S64 => return "s64".into(),
        Type::F32 => return "f32".into(),
        Type::F64 => return "f64".into(),
        Type::Char => return "char".into(),
        Type::String => return "string".into(),
        Type::ErrorContext => return "error-context".into(),
        Type::Id(id) => *id,
    };

    let def = &resolve.types[id];
    if let Some(name) = &def.name {
        return name.clone();
    }

    let opt = |t: &Option<Type>| t.as_ref().map(|t| type_name(resolve, t));
    match &def.kind {
        TypeDefKind::Option(t) => format!("option<{}>", type_name(resolve, t)),
        TypeDefKind::List(t) => format!("list<{}>", type_name(resolve, t)),
        TypeDefKind::FixedLengthList(t, n) => format!("list<{}, {n}>", type_name(resolve, t)),
        TypeDefKind::Map(k, v) => {
            format!("map<{}, {}>", type_name(resolve, k), type_name(resolve, v))
        }
        TypeDefKind::Tuple(t) => {
            let items: Vec<_> = t.types.iter().map(|t| type_name(resolve, t)).collect();
            format!("tuple<{}>", items.join(", "))
        }
        TypeDefKind::Result(r) => match (opt(&r.ok), opt(&r.err)) {
            (None, None) => "result".into(),
            (Some(ok), None) => format!("result<{ok}>"),
            (None, Some(err)) => format!("result<_, {err}>"),
            (Some(ok), Some(err)) => format!("result<{ok}, {err}>"),
        },
        TypeDefKind::Future(t) => opt(t).map_or("future".into(), |t| format!("future<{t}>")),
        TypeDefKind::Stream(t) => opt(t).map_or("stream".into(), |t| format!("stream<{t}>")),
        TypeDefKind::Handle(Handle::Own(id)) => type_name(resolve, &Type::Id(*id)),
        TypeDefKind::Handle(Handle::Borrow(id)) => {
            format!("borrow<{}>", type_name(resolve, &Type::Id(*id)))
        }
        TypeDefKind::Type(t) => type_name(resolve, t),
        // Records, variants, enums, flags and resources always have a name.
        _ => "<unnamed>".into(),
    }
}
