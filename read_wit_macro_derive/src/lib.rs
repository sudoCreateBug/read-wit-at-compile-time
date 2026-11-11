use proc_macro::TokenStream;
use std::path::PathBuf;
use syn::{LitStr, parse_macro_input};
use wit_parser::{Function, Handle, Resolve, Type, TypeDefKind, WorldItem, WorldKey};

/// Parses a WIT file (or directory of WIT files) at compile time and prints
/// its interfaces and worlds.
///
/// The path is resolved relative to the `CARGO_MANIFEST_DIR` of the crate
/// invoking the macro. Parse errors are reported as compile errors.
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

    print_resolve(&resolve);

    // `include_str!` makes cargo track each WIT file, so the macro reruns
    // whenever one of them changes.
    let tracked: String = sources
        .paths()
        .map(|p| format!("const _: &str = include_str!({:?});\n", p.display().to_string()))
        .collect();
    tracked.parse().unwrap()
}

fn print_resolve(resolve: &Resolve) {
    for (_, package) in &resolve.packages {
        eprintln!("package {}", package.name);

        eprintln!("  interfaces:");
        for name in package.interfaces.keys() {
            eprintln!("    {name}");
        }

        eprintln!("  worlds:");
        for &world_id in package.worlds.values() {
            let world = &resolve.worlds[world_id];
            eprintln!("    {}", world.name);
            print_items(resolve, "import", world.imports.iter());
            print_items(resolve, "export", world.exports.iter());
        }
    }
}

fn print_items<'a>(
    resolve: &Resolve,
    direction: &str,
    items: impl Iterator<Item = (&'a WorldKey, &'a WorldItem)>,
) {
    for (key, item) in items {
        match item {
            WorldItem::Interface { id, .. } => {
                let interface = &resolve.interfaces[*id];
                // Inline `import foo: interface { ... }` declarations have no name.
                let kind = match interface.name {
                    Some(_) => "named",
                    None => "anonymous",
                };
                eprintln!(
                    "      {direction} interface {} ({kind})",
                    resolve.name_world_key(key)
                );
                for func in interface.functions.values() {
                    print_function(resolve, "          ", func);
                }
            }
            // Functions declared directly in the world, outside any interface.
            WorldItem::Function(func) => {
                eprint!("      {direction} function ");
                print_function(resolve, "", func);
            }
            // Bare types in a world are not of interest here.
            WorldItem::Type { .. } => {}
        }
    }
}

fn print_function(resolve: &Resolve, indent: &str, func: &Function) {
    let kind = if func.kind.is_async() { "async" } else { "sync" };
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
    eprintln!(
        "{indent}{}  kind={kind}  params=({params})  result={result}",
        func.name
    );
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
