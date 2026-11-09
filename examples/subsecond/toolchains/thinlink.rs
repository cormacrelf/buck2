/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! ThinLink for subsecond, as a linker for Buck's Rust rules. Linux (ELF) only.
//!
//! This is the linker of the example's C++ toolchain, which the Rust rules use. It wraps the
//! real linker driver, and changes the link when rustc runs with `SUBSECOND_LINK` set:
//!
//! - `fat`, for the binary that is patched: link every object file of the dependencies, not
//!   only those the binary uses, keep every section, and put all symbols in the dynamic symbol
//!   table. A patch can then call any function of the dependencies.
//! - `thin`, for a patch: link only the crate's own object files into a shared library. The
//!   dependencies stay undefined, and the dynamic loader finds them in the running binary.
//!
//! Without `SUBSECOND_LINK` it runs the driver with the arguments it got, for build scripts and
//! proc macros.
//!
//! `thinlink jump-table EXE PATCH` prints the subsecond jump table for loading PATCH into a
//! process that runs EXE, as JSON, with the `lib` field left out.
//!
//! It uses only `std`, so that Buck can build it with `rustc` alone: a `rust_binary` would need
//! the toolchain that this program is part of.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitCode;

type Error = Box<dyn std::error::Error>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("jump-table") => jump_table(&args[1..]),
        _ => link(args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("thinlink: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Linking

fn link(args: Vec<String>) -> Result<ExitCode, Error> {
    let [flag, driver, rest @ ..] = args.as_slice() else {
        return Err("usage: thinlink --driver DRIVER LINKER_ARGS...".into());
    };
    if flag != "--driver" {
        return Err("usage: thinlink --driver DRIVER LINKER_ARGS...".into());
    }
    let args = expand_response_files(rest)?;
    let mode = env::var("SUBSECOND_LINK").unwrap_or_default();
    let args = match mode.as_str() {
        "fat" => fat_link_args(args)?,
        "thin" => thin_link_args(args, driver)?,
        "" => args,
        other => return Err(format!("unknown SUBSECOND_LINK `{other}`").into()),
    };
    // The arguments can be too long for one command line.
    let argsfile = scratch_dir()?.join("thinlink.args");
    fs::write(&argsfile, write_response_file(&args))?;
    let status = Command::new(driver)
        .arg(format!("@{}", argsfile.display()))
        .status()
        .map_err(|e| format!("could not run `{driver}`: {e}"))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

/// The binary: every object of every dependency, and all symbols exported.
fn fat_link_args(args: Vec<String>) -> Result<Vec<String>, Error> {
    let objects_dir = scratch_dir()?.join("fat-objects");
    let mut out = Vec::with_capacity(args.len());
    for (i, arg) in args.into_iter().enumerate() {
        // The standard library stays an archive: only the dependencies that Buck builds are
        // linked whole.
        if arg.ends_with(".rlib") && !arg.contains("/lib/rustlib/") {
            let dir = objects_dir.join(i.to_string());
            fs::create_dir_all(&dir)?;
            for object in extract_objects(Path::new(&arg), &dir)? {
                out.push(object.display().to_string());
            }
        } else {
            out.push(arg);
        }
    }
    out.extend([
        // A patch's references to the dependencies resolve to these copies.
        "-Wl,--export-dynamic".to_owned(),
        // rustc passes `--gc-sections`, which would drop the functions the binary does not use.
        "-Wl,--no-gc-sections".to_owned(),
    ]);
    Ok(out)
}

/// A patch: the crate's own objects, in a shared library that leaves the rest undefined.
fn thin_link_args(args: Vec<String>, driver: &str) -> Result<Vec<String>, Error> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let keep = if arg.ends_with("/symbols.o") {
            // rustc's list of the symbols to keep, which names the allocator shim's.
            // `--no-gc-sections` below keeps everything.
            false
        } else if arg.ends_with(".o") {
            // rustc adds an allocator shim to a shared library. It calls functions of the
            // standard library that are hidden, so the patch cannot find them in the binary.
            // Leave it out, and use the binary's shim.
            !is_allocator_shim(Path::new(&arg))?
        } else {
            // The dependencies, and the standard library, are in the binary.
            !arg.ends_with(".rlib")
                // rustc's version script makes the crate's symbols local, including `main`.
                && !arg.starts_with("-Wl,--version-script")
                // Undefined symbols are the point.
                && arg != "-Wl,-z,defs"
                && arg != "-Wl,--no-undefined"
        };
        if keep {
            out.push(arg);
        }
    }
    if !out.iter().any(|arg| arg == "-shared") {
        out.push("-shared".to_owned());
    }
    // Calls between functions of the patch go to the patch, not to the old copies in the
    // binary.
    out.push("-Wl,-Bsymbolic".to_owned());
    // Keep every function of the crate: nothing in the patch refers to them, the jump table
    // does.
    out.push("-Wl,--no-gc-sections".to_owned());
    out.push(main_object(driver)?.display().to_string());
    Ok(out)
}

/// Whether the object file at `path` is rustc's allocator shim: it defines `__rust_alloc`.
fn is_allocator_shim(path: &Path) -> Result<bool, Error> {
    Ok(Elf::read(path)?
        .symbols(SHT_SYMTAB)?
        .iter()
        .any(|s| s.section != SHN_UNDEF && s.name.ends_with("__rust_alloc")))
}

/// An object file that defines `main`. `subsecond::apply_patch` finds where the patch was
/// loaded from the address of its `main`, and `jump-table` reads that address. It is never
/// called.
fn main_object(driver: &str) -> Result<PathBuf, Error> {
    let dir = scratch_dir()?;
    let source = dir.join("thinlink_main.c");
    let object = dir.join("thinlink_main.o");
    fs::write(&source, "int main(void) { return 0; }\n")?;
    let status = Command::new(driver)
        .args(["-x", "c", "-c", "-fPIC", "-o"])
        .arg(&object)
        .arg(&source)
        .status()?;
    if !status.success() {
        return Err(format!("`{driver}` could not compile {}", source.display()).into());
    }
    Ok(object)
}

/// A directory for this link's files: Buck's scratch directory for the action, if there is one.
fn scratch_dir() -> Result<PathBuf, Error> {
    let dir = match env::var_os("BUCK_SCRATCH_PATH") {
        Some(dir) => PathBuf::from(dir).join("thinlink"),
        None => env::temp_dir().join(format!("thinlink-{}", std::process::id())),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Replace each `@file` argument with the arguments in the file, one per line, as rustc and
/// the prelude write them.
fn expand_response_files(args: &[String]) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for arg in args {
        match arg.strip_prefix('@') {
            Some(path) => {
                let content = fs::read_to_string(path)
                    .map_err(|e| format!("could not read response file `{path}`: {e}"))?;
                let lines: Vec<String> = content
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(unquote)
                    .collect();
                out.extend(expand_response_files(&lines)?);
            }
            None => out.push(arg.clone()),
        }
    }
    Ok(out)
}

/// Undo GNU response file quoting: surrounding double quotes, and backslash escapes.
fn unquote(line: &str) -> String {
    let line = line
        .strip_prefix('"')
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn write_response_file(args: &[String]) -> String {
    let mut out = String::new();
    for arg in args {
        out.push('"');
        for c in arg.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push_str("\"\n");
    }
    out
}

/// Write the object files of the GNU `ar` archive `rlib` to `dir`, and return their paths.
fn extract_objects(rlib: &Path, dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let data = fs::read(rlib).map_err(|e| format!("could not read {}: {e}", rlib.display()))?;
    let Some(mut rest) = data.strip_prefix(b"!<arch>\n") else {
        return Err(format!("{} is not an ar archive", rlib.display()).into());
    };
    let mut long_names: &[u8] = &[];
    let mut objects = Vec::new();
    while rest.len() >= 60 {
        let (header, after) = rest.split_at(60);
        let name = std::str::from_utf8(&header[..16])?.trim_end();
        let size: usize = std::str::from_utf8(&header[48..58])?.trim().parse()?;
        let member = after
            .get(..size)
            .ok_or_else(|| format!("{} is truncated", rlib.display()))?;
        rest = &after[(size + size % 2).min(after.len())..];
        if name == "//" {
            long_names = member;
            continue;
        }
        let name = match name.strip_prefix('/').map(str::parse::<usize>) {
            // A GNU long name: an offset into the `//` member, ended by "/\n".
            Some(Ok(start)) => {
                let tail = long_names
                    .get(start..)
                    .ok_or_else(|| format!("bad long name in {}", rlib.display()))?;
                let end = tail
                    .windows(2)
                    .position(|w| w == b"/\n")
                    .unwrap_or(tail.len());
                std::str::from_utf8(&tail[..end])?.to_owned()
            }
            _ => name.trim_end_matches('/').to_owned(),
        };
        if name.ends_with(".o") {
            let path = dir.join(&name);
            fs::write(&path, member)?;
            objects.push(path);
        }
    }
    Ok(objects)
}

// ---------------------------------------------------------------------------------------------
// Jump tables

fn jump_table(args: &[String]) -> Result<ExitCode, Error> {
    let [exe, patch] = args else {
        return Err("usage: thinlink jump-table EXE PATCH".into());
    };
    let exe = Elf::read(Path::new(exe))?;
    let patch = Elf::read(Path::new(patch))?;

    let old = exe.functions()?;
    let new = patch.functions()?;
    let main = |functions: &BTreeMap<String, u64>, what: &str| {
        functions
            .get("main")
            .copied()
            .ok_or_else(|| format!("{what} has no `main`"))
    };
    let aslr_reference = main(&old, "the binary")?;
    let new_base_address = main(&new, "the patch")?;

    // Rust functions that the patch calls but the binary does not export. Loading the patch
    // would fail, or calling them would crash.
    let exported = exe.dynamic_symbols(true)?;
    let missing: BTreeSet<String> = patch
        .dynamic_symbols(false)?
        .into_iter()
        .filter(|name| {
            (name.starts_with("_R") || name.starts_with("_ZN")) && !exported.contains(name)
        })
        .collect();

    let mut json = String::from("{\"map\":{");
    let mut first = true;
    for (name, old_address) in &old {
        if let Some(new_address) = new.get(name) {
            if !first {
                json.push(',');
            }
            first = false;
            json.push_str(&format!("\"{old_address}\":{new_address}"));
        }
    }
    json.push_str(&format!(
        "}},\"aslr_reference\":{aslr_reference},\"new_base_address\":{new_base_address},\"ifunc_count\":0,\"missing\":["
    ));
    for (i, name) in missing.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        // Mangled names have no characters that need escaping in JSON.
        json.push_str(&format!("\"{name}\""));
    }
    json.push_str("]}");
    println!("{json}");
    Ok(ExitCode::SUCCESS)
}

/// An ELF64 little-endian file, read for its symbol tables.
struct Elf {
    path: PathBuf,
    data: Vec<u8>,
}

struct Symbol {
    name: String,
    value: u64,
    kind: u8,
    binding: u8,
    section: u16,
}

const SHT_SYMTAB: u32 = 2;
const SHT_DYNSYM: u32 = 11;
const STT_FUNC: u8 = 2;
const STB_WEAK: u8 = 2;
const SHN_UNDEF: u16 = 0;

impl Elf {
    fn read(path: &Path) -> Result<Elf, Error> {
        let data = fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if data.get(..6) != Some(b"\x7fELF\x02\x01".as_slice()) {
            return Err(
                format!("{} is not a 64-bit little-endian ELF file", path.display()).into(),
            );
        }
        Ok(Elf {
            path: path.to_owned(),
            data,
        })
    }

    fn bytes(&self, at: u64, len: usize) -> Result<&[u8], Error> {
        let at = usize::try_from(at)?;
        self.data
            .get(at..at + len)
            .ok_or_else(|| format!("{} is truncated", self.path.display()).into())
    }

    fn u16(&self, at: u64) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.bytes(at, 2)?.try_into()?))
    }

    fn u32(&self, at: u64) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes(at, 4)?.try_into()?))
    }

    fn u64(&self, at: u64) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.bytes(at, 8)?.try_into()?))
    }

    /// The symbols of the first section of type `section_type`.
    fn symbols(&self, section_type: u32) -> Result<Vec<Symbol>, Error> {
        let shoff = self.u64(0x28)?;
        let shentsize = u64::from(self.u16(0x3a)?);
        let shnum = u64::from(self.u16(0x3c)?);
        for i in 0..shnum {
            let header = shoff + i * shentsize;
            if self.u32(header + 4)? != section_type {
                continue;
            }
            let offset = self.u64(header + 24)?;
            let size = self.u64(header + 32)?;
            let link = u64::from(self.u32(header + 40)?);
            let entsize = self.u64(header + 56)?.max(24);
            let strtab = self.u64(shoff + link * shentsize + 24)?;
            let mut symbols = Vec::new();
            for j in 0..size / entsize {
                let sym = offset + j * entsize;
                let info = self.bytes(sym + 4, 1)?[0];
                symbols.push(Symbol {
                    name: self.c_str(strtab + u64::from(self.u32(sym)?))?,
                    kind: info & 0xf,
                    binding: info >> 4,
                    section: self.u16(sym + 6)?,
                    value: self.u64(sym + 8)?,
                });
            }
            return Ok(symbols);
        }
        Ok(Vec::new())
    }

    fn c_str(&self, at: u64) -> Result<String, Error> {
        let start = usize::try_from(at)?;
        let tail = self
            .data
            .get(start..)
            .ok_or_else(|| format!("{} is truncated", self.path.display()))?;
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    }

    /// Defined functions, by name, from the full symbol table.
    fn functions(&self) -> Result<BTreeMap<String, u64>, Error> {
        Ok(self
            .symbols(SHT_SYMTAB)?
            .into_iter()
            .filter(|s| s.kind == STT_FUNC && s.section != SHN_UNDEF && !s.name.is_empty())
            .map(|s| (s.name, s.value))
            .collect())
    }

    /// Names in the dynamic symbol table: defined ones, or undefined non-weak ones.
    fn dynamic_symbols(&self, defined: bool) -> Result<BTreeSet<String>, Error> {
        Ok(self
            .symbols(SHT_DYNSYM)?
            .into_iter()
            .filter(|s| !s.name.is_empty())
            .filter(|s| {
                if defined {
                    s.section != SHN_UNDEF
                } else {
                    s.section == SHN_UNDEF && s.binding != STB_WEAK
                }
            })
            .map(|s| s.name)
            .collect())
    }
}
