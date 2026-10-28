# lib/mk-rushi-kernel.nix — kernel binary build (mkRushi step 3).
#
# Builds the rushi kernel with rustPlatform.buildRustPackage. The
# kernel's bin/ contains all stage + tool binaries, and tools/ ships
# the tool manifests. The toolchain resolves in this order:
#
#   1. `rustToolchain` — caller-supplied (e.g. fenix from the flake)
#   2. `pkgs.fenix`    — a fenix overlay is in scope
#   3. error           — points at examples/rushi-config/flake.nix
#
# Plain function (not a module): the flake `lib` output must be a
# value. Used by lib/mk-rushi.nix.

{
  pkgs,
  src,
  version,
  rustToolchain ? null,
  cargoLockContents ? null,
  kernelCargoLock ? null,
}:

let
  fenixInput = pkgs.fenix or null;
  toolchain = if rustToolchain != null then rustToolchain
    else if fenixInput != null then
      fenixInput.stable.withComponents [ "cargo" "rust-src" "rustc" "rustfmt" ]
    else
      throw "rushi.mkRushi: fenix not in pkgs and no rustToolchain passed. "
        + "Use an overlay that adds fenix (see examples/rushi-config/flake.nix).";
in
{
  rushi = pkgs.rustPlatform.buildRustPackage rec {
    pname = "rushi";
    inherit version src;
    # The kernel's Cargo.lock, in order of preference:
    #   1. kernelCargoLock — a Nix path reference to the kernel's
    #      Cargo.lock, supplied by the flake wrapper where `./Cargo.lock`
    #      resolves correctly (even when the kernel is a flake path input).
    #   2. cargoLockContents — the lock text read by the flake, fed
    #      directly to importCargoLock.
    #   3. src/Cargo.lock — the path form for direct (non-flake) callers.
    cargoLock = if kernelCargoLock != null then
      kernelCargoLock
    else if cargoLockContents != null then
      { lockFileContents = cargoLockContents; }
    else
      { lockFile = src/Cargo.lock; };
    nativeBuildInputs = [ toolchain ];
    # Build all workspace members (loop stages, tools, hooks).
    cargoBuildFlags = [ "--workspace" ];
    doCheck = false;
    # Ship tool manifests alongside bin/ so resolve_kernel_tools_dir
    # (<exe>/../tools) works without RUSHI_KERNEL.
    postInstall = ''
      mkdir -p $out/tools
      cp -r $src/tools/. $out/tools/
    '';
  };
}
