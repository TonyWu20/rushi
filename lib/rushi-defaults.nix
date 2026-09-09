# lib/rushi-defaults.nix — kernel defaults for the rushi config.
#
# These are the values that the kernel binary uses when no
# user-configured value is provided. The mkRushi module merges
# consumer-provided options on top of these defaults, so a consumer
# only needs to declare what it wants to override.

{
  version = "0.1";

  config = {
    # [active]
    active = {
      model = "";
    };

    # [model] — global model settings. Per-model overrides are
    # nested keys in this same attrset (e.g. "deepseek" = { ... }).
    model = {
      api = "chat";
      max_output_tokens = 8192;
      reasoning_effort = "medium";
      # model_timeout_s = 0;       # 0 = no cap (omit)
      # Per-model overrides:
      # "deepseek" = {
      #   model_id = "deepseek-v4-flash";
      #   base_url = "https://api.deepseek.com";
      #   api_key_env = "DEEPSEEK_API_KEY";
      #   context_tokens = 131072;
      # };
    };

    # [paths]
    paths = {
      sessions_root = "sessions";
      tools_root = "tools";
      extra_tools_roots = [ ];
    };

    # [limits]
    limits = {
      read_limit = 2000;
      read_max_line_length = 2000;
      read_max_bytes = 51200;
      read_stream_min_size = 10485760;
      write_max_bytes = 1048576;
      tool_result_max_chars = 20000;
      bash_max_output_bytes = 16000;
      bash_timeout_default = 60;
      bash_timeout_max = 300;
      compact_enabled = true;
      compact_reserve_tokens = 16384;
      compact_keep_tokens = 20000;
      compact_strategy = "compact";
      # context_budget_tokens = 0;   # 0 = auto (omit)
      # approval_timeout_s = 0;      # 0 = wait forever (omit)
      # compact_reasoning_effort = ""; # "" = inherit (omit)
    };

    # [hooks]
    hooks = {
      timeout_ms = 30000;
      on = [ ];
    };

    # [loop]
    loop = {
      command = "rushi";
      args = [ "run" ];
      arg_style = "append_session";
    };

    # [system_prompt]
    system_prompt = {
      text = "";
    };

    # [tui]
    tui = {
      binary = "";
      color = "";
      color_scheme = "";
      tool_display = {
        preset = "";
        # preview_lines = 0;
        # bash_collapsed_lines = 0;
        # diff_collapsed_lines = 0;
        # expanded_preview_max_lines = 0;
        diff_view = "auto";
      };
    };

    # [ext]
    ext = {
      dir = "";
    };
  };

  # Kernel tool names to include in the package.
  tools = [ "read" "write" "edit" "bash" ];

  # UI extension names to include.
  ui_extensions = [ ];

  # External tool sources (Nix derivations or path strings).
  external_tools = [ ];

  # External UI extension sources.
  external_ui_extensions = [ ];

  # External hook binaries.
  external_hooks = [ ];
}
