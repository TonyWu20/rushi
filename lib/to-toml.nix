# lib/to-toml.nix — minimal TOML document generator for the rushi config.
#
# Handles the specific config structure: flat sections, nested
# per-model tables, and array-of-tables ([[hooks.on]]). Not a general
# TOML serializer.
#
# Usage:
#   toTomlDocument = import ./lib/to-toml.nix;
#   tomlText = toTomlDocument configOpts;

let

  # ── Value formatting ──

  # Format a single TOML value (not a table).
  tv = v:
    if builtins.isBool v then
      if v then "true" else "false"
    else if builtins.isInt v then
      builtins.toString v
    else if builtins.isFloat v then
      builtins.toString v
    else if builtins.isString v then
      if builtins.match ".*\n.*" v != null then
        "'''" + "\n" + v + "\n'''"
      else
        let dq = "\""; in dq + v + dq
    else if builtins.isList v then
      if v == [] then
        "[]"
      else
        "[ ${builtins.concatStringsSep ", " (builtins.map tv v)} ]"
    else
      throw "to-toml.tv: unsupported value type: ${builtins.typeOf v}";

  # Quote a key if it contains characters outside [a-zA-Z0-9_-].
  tk = k:
    if builtins.match "^[a-zA-Z0-9_-]+$" k != null then
      k
    else
      let dq = "\""; in dq + k + dq;

  # Emit one "key = value" line.
  kvLine = k: v:
    "${tk k} = ${tv v}\n";

  # Emit a complete TOML section for one attrset.
  # `prefix` is the dotted path ("" for top level).
  emitSection = prefix: attrs:
    let
      keys = builtins.attrNames attrs;

      isAOT    = k: builtins.isList attrs.${k} && builtins.all (x: builtins.isAttrs x) attrs.${k} && attrs.${k} != [];
      isTable  = k: builtins.isAttrs attrs.${k};
      isScalar = k: !(isTable k) && !(isAOT k);

      scalarKeys = builtins.filter isScalar keys;
      tableKeys  = builtins.filter (k: isTable k && !(isAOT k)) keys;
      aotKeys    = builtins.filter isAOT keys;

      header = if prefix == "" then "" else "[${prefix}]\n";

      # Build scalar lines.
      scalarLines =
        builtins.concatStringsSep ""
          (builtins.map (k: kvLine k attrs.${k}) scalarKeys);

      # Build sub-table lines (recursive).
      subLines =
        builtins.concatStringsSep ""
          (builtins.map (k:
            let
              childPrefix = if prefix == "" then tk k else "${prefix}.${tk k}";
            in
              emitSection childPrefix attrs.${k}
          ) tableKeys);

      # Build array-of-tables lines.
      aotLines =
        builtins.concatStringsSep ""
          (builtins.map (k:
            let
              entries    = attrs.${k};
              fullPrefix = if prefix == "" then tk k else "${prefix}.${tk k}";
              emitEntry  = entry:
                let
                  entryLines = builtins.concatStringsSep ""
                    (builtins.map (ek: kvLine ek entry.${ek}) (builtins.attrNames entry));
                in
                  "[[${fullPrefix}]]\n" + entryLines + "\n";
            in
              builtins.concatStringsSep "" (builtins.map emitEntry entries)
          ) aotKeys);

      body = scalarLines + subLines + aotLines;
    in
      header + body;

in
cfg: emitSection "" cfg
