# Skills

Status: built. Code: `backend/src/agent/skills.rs`,
`backend/src/agent/tools/skill.rs`, `backend/src/bin/luma-mcp.rs`,
`backend/crates/mcp-stdio`.

Skills are the lighting-craft playbooks an agent reads before it authors. They
use the [Agent Skills](https://agentskills.io/specification) format.

## Files

Each skill is `resources/skills/<name>/SKILL.md`. The file starts with
frontmatter and continues with a markdown body.

Frontmatter keys:

| key | rule |
|---|---|
| `name` | Required. At most 64 characters, `[a-z0-9-]`. |
| `description` | Required. At most 1024 characters. The model sees only this text before it loads the skill. |
| `disable-model-invocation` | Optional. `true` hides the skill from the model listing. A person can still ask for it by name. |
| `allowed-tools` | Not supported. Luma enforces no tool permissions, so the registry reports a diagnostic. |

## Registry

`SkillRegistry` is the one parser. No other code reads `SKILL.md` files.

- `SkillRegistry::load(roots)` walks the roots. A directory that holds a
  `SKILL.md` is a skill. Hidden entries are skipped.
- Discovery never fails. A bad skill becomes a `SkillDiagnostic` and drops out
  of the listing. A skill without a `description` is dropped.
- `listing()` returns the `<available_skills>` block. Entries are sorted by
  name, so the bytes are the same on every run. This keeps the prompt-cache
  prefix stable.
- `get(name)` returns one skill. `Skill::envelope()` wraps the body in a
  `<skill name=… location=…>` block with the rule for relative references.
- `skills::bundled()` loads the first root that exists: `LUMA_SKILLS_ROOT`,
  then the repository `resources/skills`, then
  `$LUMA_RESOURCE_DIR/resources/skills`, then the directories beside the
  executable.

## In-app agent

- The system prompt ends with the registry listing (`backend/src/agent/mod.rs`).
- The `skill` tool takes `{name}` and returns the envelope. The tool
  description is static text from `prompts/skill-tool.md`.
- The agent's registry is `tools::registry_for_context`: `python`, `skill`, and
  `subagent` when the thread edits a score.

## MCP

`luma-mcp` gives an external agent the same skills:

- A `skill` tool. Its description holds the short instruction and the full
  listing, because an MCP client has no Luma system prompt.
- The reply to `open` says that craft guidance is available through the
  `skill` tool.
- `prompts/list` and `prompts/get`. Claude Code shows each skill as
  `/mcp__luma__<name>`. `mcp_stdio::Surface` answers these from static data.

Luma does not write skills into a client's own skill directory.

## Not built

- User skill roots, for example `$APPCONFIG/skills`.
- File access to `references/`, `scripts/` or `assets/` for the in-app agent.
  Its sandbox has no reader for those paths.
