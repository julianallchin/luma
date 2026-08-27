//! Skills: the lighting-craft playbooks, discovered from disk.
//!
//! A skill is an [Agent Skills](https://agentskills.io/specification) directory
//! — `<root>/<name>/SKILL.md`, `name` + `description` frontmatter, markdown
//! body, optional `references/`, `scripts/`, `assets/` beside it. This module is
//! the one registry: the in-app loop reads its [`SkillRegistry::listing`] into
//! the system prompt and its bodies through the `skill` tool, the webview reads
//! the same two things over the dispatch seam, and `luma-mcp` hands them to an
//! external agent. A second parse of these files anywhere would be a second
//! vocabulary.
//!
//! **A bad playbook is a diagnostic, not a failure.** Discovery never errors:
//! an unreadable directory, malformed frontmatter or an over-long name degrades
//! the listing by one entry and says why. Only a missing `description` drops a
//! skill silently-but-loudly, because the description *is* the routing signal —
//! a skill the model cannot choose is not a skill.
//!
//! Discovery policy lives above this module (which roots, in what order);
//! parsing and validation live here and only ever see a path list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Longest `name` the spec allows.
const MAX_NAME: usize = 64;
/// Longest `description` the spec allows.
const MAX_DESCRIPTION: usize = 1024;

/// One playbook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Lookup key and the name shown to the model.
    pub name: String,
    /// What the skill is for and when to reach for it — the only text a model
    /// sees before choosing to load it.
    pub description: String,
    /// The markdown instructions, verbatim minus the frontmatter block.
    pub body: String,
    /// The `SKILL.md` itself. Model-visible, and the base for resolving the
    /// relative references a body may contain.
    pub path: PathBuf,
    /// Advertised to the model, or reachable only when a human asks for it by
    /// name (`disable-model-invocation: true`).
    pub model_invocable: bool,
}

impl Skill {
    /// The skill as a tool result: Pi's `formatSkillInvocation` envelope, so a
    /// model sees the same framing here as it would in a harness that read the
    /// file itself — including the rule that makes relative references
    /// resolvable by a host that *can* read files.
    #[must_use]
    pub fn envelope(&self) -> String {
        let directory = self.path.parent().unwrap_or(&self.path);
        format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
            self.name,
            self.path.display(),
            directory.display(),
            self.body,
        )
    }
}

/// Why one directory did not become a skill (or became a suspect one).
///
/// Carries no machine-readable code: nothing branches on these, they are read
/// by a person looking at stderr, and a code that no caller matches is an
/// interface nobody pays for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// The file or directory the complaint is about.
    pub path: PathBuf,
    /// What is wrong with it, in one line.
    pub message: String,
}

impl std::fmt::Display for SkillDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.message)
    }
}

/// Every skill this process can serve, and the `<available_skills>` block that
/// advertises them.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    /// Sorted by name, which is what makes [`listing`](Self::listing)
    /// byte-stable and therefore cacheable as part of a prompt prefix.
    skills: BTreeMap<String, Skill>,
    listing: String,
}

impl SkillRegistry {
    /// Discover every skill under `roots`, in order — a later root replaces an
    /// earlier root's skill of the same name, which is how a user directory
    /// would override a bundled playbook.
    ///
    /// Never fails. A root that does not exist is skipped; anything else that
    /// goes wrong comes back as a diagnostic.
    #[must_use]
    pub fn load(roots: &[PathBuf]) -> (Self, Vec<SkillDiagnostic>) {
        let mut skills = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for root in roots {
            discover(root, &mut skills, &mut diagnostics);
        }
        let listing = render_listing(&skills);
        (Self { skills, listing }, diagnostics)
    }

    /// The `<available_skills>` block for a system prompt, or an empty string
    /// when nothing is model-invocable — a prompt should not advertise a menu
    /// with no dishes on it.
    #[must_use]
    pub fn listing(&self) -> &str {
        &self.listing
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name.trim())
    }

    /// Every skill, name-sorted.
    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    /// The names a caller may pass to [`get`](Self::get), for the "you asked
    /// for one that does not exist" message.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.skills.keys().map(String::as_str).collect()
    }
}

/// The bundled playbooks, loaded once.
///
/// Global rather than a field of `AppServices` because the surfaces that need
/// it are static: a tool's `description()` is built without a context, and the
/// system prompt is a `&'static str` so that it stays a cacheable prefix.
/// Bundled skills are read-only content shipped with the binary, so a process
/// -wide value is honest about what they are — the same class of thing as
/// `include_str!`.
pub fn bundled() -> &'static SkillRegistry {
    static BUNDLED: OnceLock<SkillRegistry> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        let (registry, diagnostics) = SkillRegistry::load(&bundled_roots());
        for diagnostic in &diagnostics {
            eprintln!("[skills] {diagnostic}");
        }
        registry
    })
}

/// Where the bundled `resources/skills` is, from wherever this binary runs.
///
/// The first candidate that exists wins; the list is empty when none does,
/// which is a registry with no skills rather than a failure to boot.
fn bundled_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("LUMA_SKILLS_ROOT") {
        candidates.push(PathBuf::from(path));
    }
    // The repo, resolved against the manifest rather than the CWD so a headless
    // host works wherever it was launched from.
    if let Some(repo) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        candidates.push(repo.join("resources/skills"));
    }
    // The bundle. Tauri maps `../resources/skills` to `_up_/resources/skills`
    // under the platform's resource directory, which sits either beside the
    // executable or one level up from it.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("_up_/resources/skills"));
            candidates.push(dir.join("../Resources/_up_/resources/skills"));
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .take(1)
        .collect()
}

/// Walk one root. A directory holding a `SKILL.md` *is* a skill and is not
/// descended into; anything else is descended into. Hidden entries are skipped.
fn discover(root: &Path, skills: &mut BTreeMap<String, Skill>, out: &mut Vec<SkillDiagnostic>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // A root that is not there is not an error: the caller offers roots, it
        // does not promise them.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            out.push(SkillDiagnostic {
                path: root.to_path_buf(),
                message: format!("could not be listed: {error}"),
            });
            return;
        }
    };
    let mut directories: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            !path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with('.'))
        })
        .collect();
    directories.sort();

    for directory in directories {
        let manifest = directory.join("SKILL.md");
        if !manifest.is_file() {
            discover(&directory, skills, out);
            continue;
        }
        match read_skill(&manifest, out) {
            Some(skill) => {
                skills.insert(skill.name.clone(), skill);
            }
            None => continue,
        }
    }
}

/// Parse and validate one `SKILL.md`. `None` when it cannot be a skill at all.
fn read_skill(manifest: &Path, out: &mut Vec<SkillDiagnostic>) -> Option<Skill> {
    let complain = |out: &mut Vec<SkillDiagnostic>, message: String| {
        out.push(SkillDiagnostic {
            path: manifest.to_path_buf(),
            message,
        });
    };
    let source = match std::fs::read_to_string(manifest) {
        Ok(source) => source,
        Err(error) => {
            complain(out, format!("could not be read: {error}"));
            return None;
        }
    };
    let (front, body) = split_frontmatter(&source);

    // `allowed-tools` is a permission claim, and this host enforces none. Pi
    // documents the field and never implements it; Claude Code's CLI is
    // reported not to enforce it either. Serving a skill that believes it is
    // sandboxed would be worse than not serving it.
    if front.contains_key("allowed-tools") {
        complain(
            out,
            "declares `allowed-tools`, which Luma does not enforce — remove it".into(),
        );
        return None;
    }

    let directory = manifest
        .parent()
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_string();
    // The spec requires name == directory; Pi warns instead, because that rule
    // is hostile to a directory shared between harnesses. Same call here.
    let name = match front.get("name").map(String::as_str).map(str::trim) {
        Some(name) if !name.is_empty() => {
            if name != directory {
                complain(
                    out,
                    format!("declares name '{name}' but lives in '{directory}'"),
                );
            }
            name.to_string()
        }
        _ => directory,
    };
    if let Err(reason) = validate_name(&name) {
        complain(out, format!("name '{name}' {reason}"));
        return None;
    }

    let description = front
        .get("description")
        .map(String::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if description.is_empty() {
        complain(
            out,
            "has no `description`, so nothing could route to it".into(),
        );
        return None;
    }
    if description.chars().count() > MAX_DESCRIPTION {
        complain(
            out,
            format!("description is longer than {MAX_DESCRIPTION} characters"),
        );
        return None;
    }
    if body.is_empty() {
        complain(out, "has no instructions under its frontmatter".into());
        return None;
    }

    Some(Skill {
        name,
        description: description.to_string(),
        body,
        path: manifest.to_path_buf(),
        model_invocable: front.get("disable-model-invocation").map(String::as_str) != Some("true"),
    })
}

/// The spec's name rule, as a sentence that completes "name '<name>' …".
fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.chars().count() > MAX_NAME {
        return Err("is longer than 64 characters");
    }
    if !name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    }) {
        return Err("may only contain lowercase letters, digits and hyphens");
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err("has a leading, trailing or doubled hyphen");
    }
    Ok(())
}

/// Split a leading `---` block of `key: value` lines from the body.
///
/// Hand-rolled rather than a YAML dependency: the two fields the spec requires
/// are flat strings, and exotic YAML in a third-party skill degrades to a
/// missing field — a diagnostic — instead of a parser panic. The shape matches
/// `src/shared/lib/agent/frontmatter.ts`, which parses the same files' siblings.
fn split_frontmatter(source: &str) -> (BTreeMap<String, String>, String) {
    let normalized = source.replace("\r\n", "\n");
    let mut front = BTreeMap::new();
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return (front, normalized.trim().to_string());
    };
    let Some((block, body)) = rest.split_once("\n---") else {
        return (front, normalized.trim().to_string());
    };
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        front.insert(key.to_string(), value.to_string());
    }
    (front, body.trim_start_matches('-').trim().to_string())
}

/// Pi's `formatSkillsForSystemPrompt`, in shape, minus its `<location>`.
///
/// Every consumer of this listing fetches by *name* through the `skill` tool —
/// none of them reads the file — so an absolute path would be dead weight that
/// also made the prompt machine-specific, defeating byte-comparison of prompts
/// across installs. The path still travels with the skill itself, in
/// [`Skill::envelope`], where a host that can read files can act on it.
fn render_listing(skills: &BTreeMap<String, Skill>) -> String {
    let mut invocable = skills
        .values()
        .filter(|skill| skill.model_invocable)
        .peekable();
    if invocable.peek().is_none() {
        return String::new();
    }
    let mut out = String::from(
        "The following skills provide specialized instructions for specific tasks.\n\
         When a task matches one's description, call `skill(name)` to load its full\n\
         instructions.\n\n\
         <available_skills>\n",
    );
    for skill in invocable {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", escape(&skill.name)));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            escape(&skill.description)
        ));
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    out
}

/// XML text escaping, per the Agent Skills standard's listing format.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, name: &str, contents: &str) {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("SKILL.md"), contents).unwrap();
    }

    fn scratch(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("luma-skills-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_directory_with_a_skill_md_is_a_skill() {
        let root = scratch("basic");
        write(
            &root,
            "color",
            "---\nname: color\ndescription: Palettes.\n---\n# Color\n\nUse fewer hues.\n",
        );
        let (registry, diagnostics) = SkillRegistry::load(&[root.clone()]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let skill = registry.get("color").unwrap();
        assert_eq!(skill.description, "Palettes.");
        assert_eq!(skill.body, "# Color\n\nUse fewer hues.");
        assert!(skill.path.ends_with("color/SKILL.md"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_bad_skill_is_a_diagnostic_not_a_failure() {
        let root = scratch("bad");
        write(
            &root,
            "good",
            "---\nname: good\ndescription: Fine.\n---\nBody.",
        );
        write(&root, "hollow", "---\nname: hollow\n---\nBody.");
        write(
            &root,
            "Shouty",
            "---\nname: Shouty\ndescription: d\n---\nBody.",
        );
        write(
            &root,
            "gated",
            "---\nname: gated\ndescription: d\nallowed-tools: Bash(git:*)\n---\nBody.",
        );
        write(&root, "empty", "---\nname: empty\ndescription: d\n---\n");
        let (registry, diagnostics) = SkillRegistry::load(&[root.clone()]);
        assert_eq!(registry.names(), vec!["good"]);
        assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
        let text = diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("no `description`"), "{text}");
        assert!(text.contains("lowercase letters"), "{text}");
        assert!(text.contains("allowed-tools"), "{text}");
        assert!(text.contains("no instructions"), "{text}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_root_is_skipped_and_a_later_root_wins() {
        let first = scratch("first");
        let second = scratch("second");
        write(
            &first,
            "color",
            "---\nname: color\ndescription: One.\n---\nA.",
        );
        write(
            &second,
            "color",
            "---\nname: color\ndescription: Two.\n---\nB.",
        );
        let (registry, diagnostics) = SkillRegistry::load(&[
            PathBuf::from("/no/such/skills"),
            first.clone(),
            second.clone(),
        ]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(registry.get("color").unwrap().body, "B.");
        std::fs::remove_dir_all(&first).unwrap();
        std::fs::remove_dir_all(&second).unwrap();
    }

    #[test]
    fn the_name_may_differ_from_the_directory_but_says_so() {
        let root = scratch("mismatch");
        write(
            &root,
            "folder",
            "---\nname: other\ndescription: d\n---\nBody.",
        );
        let (registry, diagnostics) = SkillRegistry::load(&[root.clone()]);
        assert_eq!(registry.names(), vec!["other"]);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("lives in 'folder'"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_listing_is_sorted_xml_escaped_and_hides_user_only_skills() {
        let root = scratch("listing");
        write(
            &root,
            "zulu",
            "---\nname: zulu\ndescription: Last & least.\n---\nZ.",
        );
        write(
            &root,
            "alpha",
            "---\nname: alpha\ndescription: First.\n---\nA.",
        );
        write(
            &root,
            "quiet",
            "---\nname: quiet\ndescription: Hidden.\ndisable-model-invocation: true\n---\nQ.",
        );
        let (registry, _) = SkillRegistry::load(&[root.clone()]);
        let listing = registry.listing();
        assert!(listing.contains("<name>alpha</name>"));
        assert!(listing.contains("Last &amp; least."));
        assert!(!listing.contains("quiet"), "{listing}");
        // No `<location>`: the prompt must be identical on every install, so a
        // machine-specific path may not enter it. The path travels in the
        // envelope instead.
        assert!(!listing.contains("SKILL.md"), "{listing}");
        assert!(!listing.contains(&root.display().to_string()), "{listing}");
        assert!(
            listing.find("alpha").unwrap() < listing.find("zulu").unwrap(),
            "entries are name-sorted so the prompt prefix is byte-stable"
        );
        // Still reachable by name, just not advertised.
        assert!(registry.get("quiet").is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_empty_registry_advertises_nothing() {
        let (registry, diagnostics) = SkillRegistry::load(&[]);
        assert!(registry.listing().is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn the_envelope_carries_the_location_and_the_reference_rule() {
        let root = scratch("envelope");
        write(
            &root,
            "color",
            "---\nname: color\ndescription: d\n---\nBody.",
        );
        let (registry, _) = SkillRegistry::load(&[root.clone()]);
        let envelope = registry.get("color").unwrap().envelope();
        assert!(envelope.starts_with("<skill name=\"color\" location=\""));
        assert!(envelope.contains("References are relative to "));
        assert!(envelope.ends_with("Body.\n</skill>"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The shipped playbooks are the reason this module exists; a broken one
    /// must fail CI rather than quietly shrink the agent's craft.
    #[test]
    fn the_bundled_playbooks_all_load() {
        let (registry, diagnostics) = SkillRegistry::load(&bundled_roots());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(registry.get("heavy-bass").is_some());
        assert!(registry.get("finding-things-in-audio").is_some());
        assert_eq!(registry.iter().count(), 10);
        assert!(registry.listing().contains("<available_skills>"));
        assert!(
            !registry.listing().contains("SKILL.md")
                && !registry.listing().contains("resources/skills"),
            "the shipped listing carries no filesystem path, so two installs \
             produce byte-identical prompts"
        );
    }
}
