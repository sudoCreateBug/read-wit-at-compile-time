use proc_macro::TokenStream;
use std::path::PathBuf;
use syn::{LitStr, parse_macro_input};
use wit_parser::{
    Docs, Function, FunctionKind, Handle, InterfaceId, Resolve, Span, Type, TypeDefKind, TypeId,
    TypeOwner, WorldId, WorldItem, WorldKey,
};

const ANNOTATION_PREFIX: &str = "@dist_gen";

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
        //It will indicate the path literal in the macro params as error, the message holds the
        //location in the WIT file where there was an error.
        let error = syn::Error::new(lit.span(), error).to_compile_error();
        output.extend(TokenStream::from(error));
    }
    output
}

struct Printer<'a> {
    resolve: &'a Resolve,
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

    fn interface_annotations(&mut self, id: InterfaceId) -> Vec<String> {
        let interface = &self.resolve.interfaces[id];
        let what = match &interface.name {
            Some(name) => format!("interface `{name}`"),
            None => "anonymous interface".to_string(),
        };
        self.parse(&interface.docs, Some(interface.span), &what)
    }

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

    fn print_type(&mut self, id: TypeId, indent: &str, functions: &[&Function]) {
        let resolve = self.resolve;
        let def = &resolve.types[id];
        let name = def.name.as_deref().unwrap_or("<unnamed>");
        let ann = self.annotations(&def.docs, Some(def.span), &format!("type `{name}`"));

        // Both `type a = b;` and `use`d types are aliases; show the type they
        // point to directly as well as the definition at the end of the chain.
        let kind = match &def.kind {
            TypeDefKind::Type(t) => format!(
                "alias of {}, true type: {}",
                qualified_name(resolve, t),
                true_type(resolve, t)
            ),
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
                    let field = format!("{}: {}", f.name, wit_type_name(resolve, &f.ty));
                    member("field", &field, &f.docs);
                }
            }
            TypeDefKind::Variant(v) => {
                for c in &v.cases {
                    let case = match &c.ty {
                        Some(ty) => format!("{}({})", c.name, wit_type_name(resolve, ty)),
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
            .map(|p| format!("{}: {}", p.name, wit_type_name(resolve, &p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let result = func
            .result
            .as_ref()
            .map_or("-".to_string(), |ty| wit_type_name(resolve, ty));
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
fn parse_annotations(doc_string: &str) -> Result<Vec<String>, String> {
    let opener = format!("{ANNOTATION_PREFIX}(");
    let mut found = Vec::new();
    let mut rest = doc_string;

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

        found.push(annotation);
    }
    Ok(found)
}

/// Follows a chain of aliases (`type a = b;`, `use x.{b}`) to the type that
/// is actually defined, rendered as e.g. `record types.ingredient` or `u32`.
fn true_type(resolve: &Resolve, ty: &Type) -> String {
    let mut ty = *ty;
    while let Type::Id(id) = ty
        && let TypeDefKind::Type(inner) = resolve.types[id].kind
    {
        ty = inner;
    }
    match ty {
        Type::Id(id) if resolve.types[id].name.is_some() => {
            let kind = resolve.types[id].kind.as_str();
            format!("{kind} {}", qualified_name(resolve, &ty))
        }
        _ => wit_type_name(resolve, &ty),
    }
}

/// prefixes named types with the interface or world that defines them, e.g. `types.ingredient`.
fn qualified_name(resolve: &Resolve, ty: &Type) -> String {
    let name = wit_type_name(resolve, ty);

    let id = match ty {
        Type::Id(id) => id,
        _ => return name,
    };

    let def = &resolve.types[*id];
    if def.name.is_none() {
        return name;
    }
    let owner = match def.owner {
        TypeOwner::Interface(i) => resolve.interfaces[i].name.clone(),
        TypeOwner::World(w) => Some(resolve.worlds[w].name.clone()),
        TypeOwner::None => None,
    };
    match owner {
        Some(owner) => format!("{owner}.{name}"),
        None => name,
    }
}

fn wit_type_name(resolve: &Resolve, ty: &Type) -> String {
    //return primitive type immediatly if known, if Type::Id(id) is found it is not a primitive type
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

    //return named types (typedefs like: type grams = u32; resources, variants, enums, ...)
    let def = &resolve.types[id];
    match &def.name {
        Some(name) => return name.clone(),
        None => {}
    }

    //resolve anonymous types (defined inline / return types such as: list, option, ...)
    let opt = |t: &Option<Type>| t.as_ref().map(|t| wit_type_name(resolve, t));
    match &def.kind {
        TypeDefKind::Option(t) => format!("option<{}>", wit_type_name(resolve, t)),
        TypeDefKind::List(t) => format!("list<{}>", wit_type_name(resolve, t)),
        TypeDefKind::FixedLengthList(t, n) => format!("list<{}, {n}>", wit_type_name(resolve, t)),
        TypeDefKind::Map(k, v) => {
            format!("map<{}, {}>", wit_type_name(resolve, k), wit_type_name(resolve, v))
        }
        TypeDefKind::Tuple(t) => {
            let items: Vec<_> = t.types.iter().map(|t| wit_type_name(resolve, t)).collect();
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
        TypeDefKind::Handle(Handle::Own(id)) => wit_type_name(resolve, &Type::Id(*id)),
        TypeDefKind::Handle(Handle::Borrow(id)) => {
            format!("borrow<{}>", wit_type_name(resolve, &Type::Id(*id)))
        }
        TypeDefKind::Type(t) => wit_type_name(resolve, t),
        // Records, variants, enums, flags and resources always have a name.
        _ => "<unnamed>".into(),
    }
}
