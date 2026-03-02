# Claude Code Skills Manager

A TUI checkbox tool for enabling/disabling Claude Code skills. Skills disabled here won't be loaded into the system prompt on new sessions.

## Why

`~/.claude/skills/` can accumulate hundreds of skill directories. Each skill's YAML frontmatter (~100 tokens) loads into every session's system prompt. This tool lets you disable skills you don't need and re-enable them when you do.

## How It Works

- **Disable**: `mv ~/.claude/skills/<name> ~/.claude/skills/.disabled/<name>`
- **Enable**: `mv ~/.claude/skills/.disabled/<name> ~/.claude/skills/<name>`

Claude Code doesn't scan `.disabled/`, so moved skills won't load.

## Install

```bash
cargo build --release
cp target/release/skills-toggle ~/.local/bin/
# or add an alias
alias skills='~/path/to/skills-toggle'
```

## Usage

### Interactive (TUI)

```bash
skills-toggle
```

Opens a full-screen checkbox list:

```
  Claude Code Skills Manager

  > [x] bun
    [x] claude-skills-basics     *
    [ ] actix-web-basics         *
    [ ] actix-web-database       *
    [x] deep-modules
    [x] nextjs

  Enabled: 280  |  Disabled: 35  |  Changed: 4  [1/315]
  ↑/↓:Navigate  Space:Toggle  a:All  n:None  /:Filter  Enter:Apply  q:Quit
```

- `[x]` green = enabled, `[ ]` red = disabled
- `*` yellow = changed this session
- Changes only apply when you press Enter and confirm

### Keys

| Key | Action |
|-----|--------|
| `↑/↓` or `j/k` | Navigate |
| `Space` | Toggle current skill |
| `PgUp/PgDn` | Page up/down |
| `g` / `G` | Jump to top / bottom |
| `a` | Enable all (filtered) |
| `n` | Disable all (filtered) |
| `/` | Search filter |
| `Enter` | Review & apply changes |
| `q` / `Esc` | Quit without saving |

### Batch Mode

```bash
# Disable all actix skills
skills-toggle disable 'actix-*'

# Enable all flutter skills
skills-toggle enable 'flutter-*'

# Multiple patterns at once
skills-toggle disable 'axum-*' 'tokio-*'

# Specific skills by name
skills-toggle disable actix-web-basics actix-web-database

# Preview without executing
skills-toggle disable 'spring-*' --dry-run
```

Supports `*` (any chars) and `?` (single char) glob patterns. Case-insensitive.

### Non-interactive

```bash
# List all skills and status
skills-toggle --list

# Interactive TUI but don't execute (show mv commands)
skills-toggle --dry-run
```

## Safety

- Toggle operations only change in-memory state
- Changes require explicit Enter + confirmation to apply
- `q` / `Esc` / `Ctrl+C` exits without saving
- Alternate screen buffer preserves your terminal scrollback
- Terminal state is always restored on exit (including panics)
