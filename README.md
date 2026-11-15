# read_wit_macro

An experiment in reading WIT files at compile time from a Rust procedural macro.

The macro `read_wit!("wit")` parses every WIT file in the given directory (relative to the
crate's `Cargo.toml`) with [`wit-parser`](https://crates.io/crates/wit-parser). It walks
the result and prints its structure to stderr during the build: packages, interfaces,
worlds, their imports and exports, types, members and functions. Alongside each item it
prints the `@dist_gen(...)` annotations found in that item's doc comments.

It does not generate any code yet. The goal is to check that a macro can find the
information a code generator would need, and the annotations attached to it.

## Structure

```
.
├── src/main.rs                 #read_wit_macro_derive::read_wit!("wit");
├── wit/bakery.wit              # example WIT package that covers the cases above
└── read_wit_macro_derive/      # proc-macro crate
    └── src/lib.rs              # the read_wit! macro, the printer and the annotation parser
```

The `_derive` suffix on the macro crate is a leftover from an earlier iteration. `read_wit!` is a
function-like procedural macro, not a derive macro.

## Running

```sh
cargo build
```

## output
```sh
package example:bakery  => @dist_gen(version = 1)
    interfaces:
        types
            type grams (alias of u32, true type: u32)  => @dist_gen(unit = "g")
            type ingredient (record)  => @dist_gen(serializable)
                field name: string  => @dist_gen(key)
                field grams: grams
            type crust (enum)
                case soft
                case crispy  => @dist_gen(default)
            type loaf (record)
                field name: string
                field weight-grams: grams
        pantry
            type ingredient (alias of types.ingredient, true type: record types.ingredient)
            type grams (alias of types.grams, true type: u32)
            type stock-level (alias of pantry.grams, true type: u32)
            function stock  kind=sync  params=(name: string)  result=stock-level  => @dist_gen(idempotent, timeout = 10, remote = "storage-room")
            function take  kind=sync  params=(name: string, amount: grams)  result=option<ingredient>
        oven  => @dist_gen(remote = "back-kitchen")
            type ingredient (alias of types.ingredient, true type: record types.ingredient)
            type bread (alias of types.loaf, true type: record types.loaf)
            type baking-tin (resource)  => @dist_gen(stateful)
                constructor constructor  kind=sync  params=(size: u32)  result=baking-tin
                method fill  kind=sync  params=(self: borrow<baking-tin>, ingredients: list<ingredient>)  result=-  => @dist_gen(idempotent)
                method is-full  kind=sync  params=(self: borrow<baking-tin>)  result=bool
                static largest  kind=sync  params=()  result=u32  => @dist_gen(cached)
            function bake  kind=async  params=(ingredients: list<ingredient>, minutes: u32, celsius: u32)  result=result<bread, string>  => @dist_gen(timeout = 60), @dist_gen(retries(max = 3), label = "a (b)")
        recipes
            type ingredient (alias of types.ingredient, true type: record types.ingredient)
            function list-recipes  kind=sync  params=()  result=list<string>
            function ingredients-for  kind=sync  params=(recipe: string)  result=option<list<ingredient>>
    worlds:
        baker  => @dist_gen(entry)
            import interface example:bakery/types (named)
                type grams (alias of u32, true type: u32)  => @dist_gen(unit = "g")
                type ingredient (record)  => @dist_gen(serializable)
                    field name: string  => @dist_gen(key)
                    field grams: grams
                type crust (enum)
                    case soft
                    case crispy  => @dist_gen(default)
                type loaf (record)
                    field name: string
                    field weight-grams: grams
            import interface example:bakery/pantry (named)
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                type grams (alias of types.grams, true type: u32)
                type stock-level (alias of pantry.grams, true type: u32)
                function stock  kind=sync  params=(name: string)  result=stock-level  => @dist_gen(idempotent, timeout = 10, remote = "storage-room")
                function take  kind=sync  params=(name: string, amount: grams)  result=option<ingredient>
            import interface example:bakery/oven (named)  => @dist_gen(optional), @dist_gen(remote = "back-kitchen")
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                type bread (alias of types.loaf, true type: record types.loaf)
                type baking-tin (resource)  => @dist_gen(stateful)
                    constructor constructor  kind=sync  params=(size: u32)  result=baking-tin
                    method fill  kind=sync  params=(self: borrow<baking-tin>, ingredients: list<ingredient>)  result=-  => @dist_gen(idempotent)
                    method is-full  kind=sync  params=(self: borrow<baking-tin>)  result=bool
                    static largest  kind=sync  params=()  result=u32  => @dist_gen(cached)
                function bake  kind=async  params=(ingredients: list<ingredient>, minutes: u32, celsius: u32)  result=result<bread, string>  => @dist_gen(timeout = 60), @dist_gen(retries(max = 3), label = "a (b)")
            import interface scale (anonymous)
                function weigh  kind=sync  params=(ingredient: string)  result=u32
            import function note  kind=sync  params=(message: string)  result=-  => @dist_gen(local)
            export function loaves-baked  kind=async  params=()  result=u32
            export interface example:bakery/recipes (named)
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                function list-recipes  kind=sync  params=()  result=list<string>
                function ingredients-for  kind=sync  params=(recipe: string)  result=option<list<ingredient>>
        shop
            import interface example:bakery/types (named)
                type grams (alias of u32, true type: u32)  => @dist_gen(unit = "g")
                type ingredient (record)  => @dist_gen(serializable)
                    field name: string  => @dist_gen(key)
                    field grams: grams
                type crust (enum)
                    case soft
                    case crispy  => @dist_gen(default)
                type loaf (record)
                    field name: string
                    field weight-grams: grams
            import interface example:bakery/recipes (named)
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                function list-recipes  kind=sync  params=()  result=list<string>
                function ingredients-for  kind=sync  params=(recipe: string)  result=option<list<ingredient>>
            export interface example:bakery/pantry (named)
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                type grams (alias of types.grams, true type: u32)
                type stock-level (alias of pantry.grams, true type: u32)
                function stock  kind=sync  params=(name: string)  result=stock-level  => @dist_gen(idempotent, timeout = 10, remote = "storage-room")
                function take  kind=sync  params=(name: string, amount: grams)  result=option<ingredient>
            export interface example:bakery/oven (named)  => @dist_gen(remote = "back-kitchen")
                type ingredient (alias of types.ingredient, true type: record types.ingredient)
                type bread (alias of types.loaf, true type: record types.loaf)
                type baking-tin (resource)  => @dist_gen(stateful)
                    constructor constructor  kind=sync  params=(size: u32)  result=baking-tin
                    method fill  kind=sync  params=(self: borrow<baking-tin>, ingredients: list<ingredient>)  result=-  => @dist_gen(idempotent)
                    method is-full  kind=sync  params=(self: borrow<baking-tin>)  result=bool
                    static largest  kind=sync  params=()  result=u32  => @dist_gen(cached)
                function bake  kind=async  params=(ingredients: list<ingredient>, minutes: u32, celsius: u32)  result=result<bread, string>  => @dist_gen(timeout = 60), @dist_gen(retries(max = 3), label = "a (b)")
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```
