# Duplicate symbol checker

This tool is intended for determining how much of a compiled Rust binary is composed of identical
functions.

It relies on debug symbols to find functions, so your binary needs to not have been stripped.

It works by reading the instructions for each function, normalising them in order to accommodate
differences that are only due to the base address of the function, then grouping by the resulting
instruction bytes.

It currently only supports x86_64 binaries and has only been tested on Linux.

Identified duplicate functions have a few different sources:

- Functions could be identical by chance even though they come from different parts of the codebase.
- Generic functions, once monomorphised, could be identical despite having different generic
  arguments. For example, the function might only depend on the size of the generic argument,
  meaning that all monomorphisations with types of the same size would end up equal.
- Generic functions might be monomorphised multiple times with the same arguments, but then not get
  deduplicated.

The last of these is what I'm most interested in. Unfortunately it's currently kind of hard to
separate these last two cases. Rustc seems to sometimes emit symbol names that include the generic
arguments, but often it emits symbol names that just have the placeholders, then uses a different
hash at the end of the symbol.

Recommended usage:

```sh
cargo run --release -- --verbose --demangle /path/to/bin
```

## Sample output

I'll now show some sample outputs from running the tool on a release build of ripgrep. I don't
include all the output since it's quite long, just a few bits that warrant further discussion.

```
Function size: 103
Copies: 21
Excess bytes: 2060
Names:
  1x `alloc::sync::Arc<T,A>::drop_slow::hd706a4fa915b4d89`
  1x `alloc::sync::Arc<T,A>::drop_slow::h87093f1f9dea2d0e`
  1x `alloc::sync::Arc<T,A>::drop_slow::h10d0dcce72958fd8`
  1x `alloc::sync::Arc<T,A>::drop_slow::h8c617ac2d907e2aa`
  1x `alloc::sync::Arc<T,A>::drop_slow::hd71eeda01817a536`
  1x `alloc::sync::Arc<T,A>::drop_slow::he520a5dd6ca64703`
  1x `alloc::sync::Arc<T,A>::drop_slow::h628a33a33ecac575`
  1x `alloc::sync::Arc<T,A>::drop_slow::h8cec9ca0439c3711`
  1x `alloc::sync::Arc<T,A>::drop_slow::h4cd5ea407012db46`
  1x `alloc::sync::Arc<T,A>::drop_slow::h224d6f2371018a1c`
  1x `alloc::sync::Arc<T,A>::drop_slow::h990f0b7fc7e3af11`
  1x `alloc::sync::Arc<T,A>::drop_slow::h73ba588d5943ac7a`
  1x `alloc::sync::Arc<T,A>::drop_slow::h2dc0bbd1c9c62e26`
  1x `alloc::sync::Arc<T,A>::drop_slow::h517054e3fb2dbac5`
  1x `alloc::sync::Arc<T,A>::drop_slow::hdf2cd5f474fa2393`
  1x `alloc::sync::Arc<T,A>::drop_slow::h61f7d6c3b84da1e9`
  1x `alloc::sync::Arc<T,A>::drop_slow::h2487201382634f65`
  1x `alloc::sync::Arc<T,A>::drop_slow::hfd3d412a64e719d1`
  1x `alloc::sync::Arc<T,A>::drop_slow::h127b8fb68b2d8622`
  1x `alloc::sync::Arc<T,A>::drop_slow::h545a994a083aa1dc`
  1x `alloc::sync::Arc<T,A>::drop_slow::he1370168d3fba403`
```

Here we can see that there's 21 copies of a function for dropping an Arc. We don't know what's in
the Arc. It's likely that each of these functions was created to drop an `Arc<T>` with a different
`T`, but that the machine code ended up identical.

```
Function size: 236
Copies: 26
Excess bytes: 5900
Names:
  3x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h0ede7a90cb4e4caf`
  3x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h3dc26697a761e8f9`
  3x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h3801b4f9aaad7fc2`
  5x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h9eb8ddad156565f6`
  5x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::hc517e495ab2a88a4`
  4x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::hde4f9e64fcdf6bea`
  3x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::hd22a22559e751911`
```

Here we have 7 different function names, differing only in their hash. The compiler has substituted
the type parameter, so we know that these are all dropping the same type. Each function name then
has several copies. These extra copies of each symbol come from separate codegen units. We can
determine this by rebuilding the binary with `codegen-units=1`, then we get the following:

```
Function size: 236
Copies: 7
Excess bytes: 1416
Names:
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h92c782dcb35669b7`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h67d0912fdfd31563`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h11e9189330999400`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h3242a4cd1700d56b`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h17293025138ff46d`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h085ce04cd90a2c3f`
  1x `core::ptr::drop_in_place<regex_automata::meta::wrappers::PikeVMCache>::h419132e2cb015325`
```

Each of these 7 copies was monomorphised when compiling a different crate. We can verify this by
running the tool without `--demangle` then using grep to locate the .rlib that contains that symbol.
Each of these symbols shows up in a different rlib.

## Typical results

For a release build of ripgrep with rustc 1.78.0, I observe about 5% of the executable bytes in the
binary are excess copies of duplicated functions.

For a debug build of ripgrep, this increases to 6%. This is not surprising, since there's less
inlining happening, so more scope for generic functions to be duplicates.

If I set `codegen-units=1` on a release build of ripgrep, then the excess copies drops to 1% of
function bytes.

For a binary with lots more dependencies, I pick on my own crate, the Rust REPL evcxr. In its
release build, 10% of executable bytes are from excess copies of duplicated functions. This drops to
1% with `codegen-units=1`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
Wild by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
