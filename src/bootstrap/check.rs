//! Implementation of compiling the compiler and standard library, in "check"-based modes.

use crate::compile::{add_to_sysroot, run_cargo, rustc_cargo, std_cargo};
use crate::config::TargetSelection;
use crate::tool::{prepare_tool_cargo, SourceType};
use crate::{
    builder::{Builder, Kind, RunConfig, ShouldRun, Step},
    Subcommand,
};
use crate::{Compiler, Mode};
use std::path::PathBuf;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Std {
    pub target: TargetSelection,
}

fn args(kind: Kind) -> Vec<String> {
    match kind {
        Kind::Clippy => vec!["--".to_owned(), "--cap-lints".to_owned(), "warn".to_owned()],
        _ => Vec::new(),
    }
}

fn cargo_subcommand(kind: Kind) -> &'static str {
    match kind {
        Kind::Check => "check",
        Kind::Clippy => "clippy",
        Kind::Fix => "fix",
        _ => unreachable!(),
    }
}

impl Step for Std {
    type Output = ();
    const DEFAULT: bool = true;

    fn should_run(run: ShouldRun<'_>) -> ShouldRun<'_> {
        run.all_krates("test")
    }

    fn make_run(run: RunConfig<'_>) {
        run.builder.ensure(Std { target: run.target });
    }

    fn run(self, builder: &Builder<'_>) {
        let target = self.target;
        let compiler = builder.compiler(0, builder.config.build);

        let mut cargo = builder.cargo(
            compiler,
            Mode::Std,
            SourceType::InTree,
            target,
            cargo_subcommand(builder.kind),
        );
        std_cargo(builder, target, compiler.stage, &mut cargo);

        builder.info(&format!("Checking std artifacts ({} -> {})", &compiler.host, target));
        run_cargo(
            builder,
            cargo,
            args(builder.kind),
            &libstd_stamp(builder, compiler, target),
            vec![],
            true,
        );

        let libdir = builder.sysroot_libdir(compiler, target);
        let hostdir = builder.sysroot_libdir(compiler, compiler.host);
        add_to_sysroot(&builder, &libdir, &hostdir, &libstd_stamp(builder, compiler, target));

        // Then run cargo again, once we've put the rmeta files for the library
        // crates into the sysroot. This is needed because e.g., core's tests
        // depend on `libtest` -- Cargo presumes it will exist, but it doesn't
        // since we initialize with an empty sysroot.
        //
        // Currently only the "libtest" tree of crates does this.

        if let Subcommand::Check { all_targets: true, .. } = builder.config.cmd {
            let mut cargo = builder.cargo(
                compiler,
                Mode::Std,
                SourceType::InTree,
                target,
                cargo_subcommand(builder.kind),
            );
            std_cargo(builder, target, compiler.stage, &mut cargo);
            cargo.arg("--all-targets");

            // Explicitly pass -p for all dependencies krates -- this will force cargo
            // to also check the tests/benches/examples for these crates, rather
            // than just the leaf crate.
            for krate in builder.in_tree_crates("test") {
                cargo.arg("-p").arg(krate.name);
            }

            builder.info(&format!(
                "Checking std test/bench/example targets ({} -> {})",
                &compiler.host, target
            ));
            run_cargo(
                builder,
                cargo,
                args(builder.kind),
                &libstd_test_stamp(builder, compiler, target),
                vec![],
                true,
            );
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Rustc {
    pub target: TargetSelection,
}

impl Step for Rustc {
    type Output = ();
    const ONLY_HOSTS: bool = true;
    const DEFAULT: bool = true;

    fn should_run(run: ShouldRun<'_>) -> ShouldRun<'_> {
        run.all_krates("rustc-main")
    }

    fn make_run(run: RunConfig<'_>) {
        run.builder.ensure(Rustc { target: run.target });
    }

    /// Builds the compiler.
    ///
    /// This will build the compiler for a particular stage of the build using
    /// the `compiler` targeting the `target` architecture. The artifacts
    /// created will also be linked into the sysroot directory.
    fn run(self, builder: &Builder<'_>) {
        let compiler = builder.compiler(0, builder.config.build);
        let target = self.target;

        builder.ensure(Std { target });

        let mut cargo = builder.cargo(
            compiler,
            Mode::Rustc,
            SourceType::InTree,
            target,
            cargo_subcommand(builder.kind),
        );
        rustc_cargo(builder, &mut cargo, target);
        if let Subcommand::Check { all_targets: true, .. } = builder.config.cmd {
            cargo.arg("--all-targets");
        }

        // Explicitly pass -p for all compiler krates -- this will force cargo
        // to also check the tests/benches/examples for these crates, rather
        // than just the leaf crate.
        for krate in builder.in_tree_crates("rustc-main") {
            cargo.arg("-p").arg(krate.name);
        }

        builder.info(&format!("Checking compiler artifacts ({} -> {})", &compiler.host, target));
        run_cargo(
            builder,
            cargo,
            args(builder.kind),
            &librustc_stamp(builder, compiler, target),
            vec![],
            true,
        );

        let libdir = builder.sysroot_libdir(compiler, target);
        let hostdir = builder.sysroot_libdir(compiler, compiler.host);
        add_to_sysroot(&builder, &libdir, &hostdir, &librustc_stamp(builder, compiler, target));
    }
}

macro_rules! tool_check_step {
    ($name:ident, $path:expr, $source_type:expr) => {
        #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            pub target: TargetSelection,
        }

        impl Step for $name {
            type Output = ();
            const ONLY_HOSTS: bool = true;
            const DEFAULT: bool = true;

            fn should_run(run: ShouldRun<'_>) -> ShouldRun<'_> {
                run.path($path)
            }

            fn make_run(run: RunConfig<'_>) {
                run.builder.ensure($name { target: run.target });
            }

            fn run(self, builder: &Builder<'_>) {
                let compiler = builder.compiler(0, builder.config.build);
                let target = self.target;

                builder.ensure(Rustc { target });

                let mut cargo = prepare_tool_cargo(
                    builder,
                    compiler,
                    Mode::ToolRustc,
                    target,
                    cargo_subcommand(builder.kind),
                    $path,
                    $source_type,
                    &[],
                );

                if let Subcommand::Check { all_targets: true, .. } = builder.config.cmd {
                    cargo.arg("--all-targets");
                }

                builder.info(&format!(
                    "Checking {} artifacts ({} -> {})",
                    stringify!($name).to_lowercase(),
                    &compiler.host.triple,
                    target.triple
                ));
                run_cargo(
                    builder,
                    cargo,
                    args(builder.kind),
                    &stamp(builder, compiler, target),
                    vec![],
                    true,
                );

                let libdir = builder.sysroot_libdir(compiler, target);
                let hostdir = builder.sysroot_libdir(compiler, compiler.host);
                add_to_sysroot(&builder, &libdir, &hostdir, &stamp(builder, compiler, target));

                /// Cargo's output path in a given stage, compiled by a particular
                /// compiler for the specified target.
                fn stamp(
                    builder: &Builder<'_>,
                    compiler: Compiler,
                    target: TargetSelection,
                ) -> PathBuf {
                    builder
                        .cargo_out(compiler, Mode::ToolRustc, target)
                        .join(format!(".{}-check.stamp", stringify!($name).to_lowercase()))
                }
            }
        }
    };
}

tool_check_step!(Rustdoc, "src/tools/rustdoc", SourceType::InTree);
// Clippy is a hybrid. It is an external tool, but uses a git subtree instead
// of a submodule. Since the SourceType only drives the deny-warnings
// behavior, treat it as in-tree so that any new warnings in clippy will be
// rejected.
tool_check_step!(Clippy, "src/tools/clippy", SourceType::InTree);

tool_check_step!(Bootstrap, "src/bootstrap", SourceType::InTree);

/// Cargo's output path for the standard library in a given stage, compiled
/// by a particular compiler for the specified target.
fn libstd_stamp(builder: &Builder<'_>, compiler: Compiler, target: TargetSelection) -> PathBuf {
    builder.cargo_out(compiler, Mode::Std, target).join(".libstd-check.stamp")
}

/// Cargo's output path for the standard library in a given stage, compiled
/// by a particular compiler for the specified target.
fn libstd_test_stamp(
    builder: &Builder<'_>,
    compiler: Compiler,
    target: TargetSelection,
) -> PathBuf {
    builder.cargo_out(compiler, Mode::Std, target).join(".libstd-check-test.stamp")
}

/// Cargo's output path for librustc in a given stage, compiled by a particular
/// compiler for the specified target.
fn librustc_stamp(builder: &Builder<'_>, compiler: Compiler, target: TargetSelection) -> PathBuf {
    builder.cargo_out(compiler, Mode::Rustc, target).join(".librustc-check.stamp")
}
