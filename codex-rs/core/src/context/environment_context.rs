use crate::context::world_state::environment_limits::MAX_RENDERED_NETWORK_DOMAINS;
use crate::context::world_state::environment_limits::MAX_RENDERED_WORKSPACE_ROOTS;
use crate::context::world_state::environment_limits::bound_entries;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_utils_path_uri::PathUri;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileSystemContext {
    workspace_roots: Vec<String>,
    omitted_workspace_roots: usize,
    permission_profile: FileSystemPermissionProfileContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileSystemPermissionProfileContext {
    Managed(ManagedFileSystemContext),
    Disabled,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagedFileSystemContext {
    Restricted {
        entries: Vec<FileSystemSandboxEntry>,
        glob_scan_max_depth: Option<usize>,
    },
    Unrestricted,
}

impl FileSystemContext {
    pub(super) fn from_permission_profile(
        permission_profile: &PermissionProfile,
        workspace_roots: &[PathUri],
    ) -> Self {
        let permission_profile = permission_profile
            .clone()
            .materialize_project_roots_with_path_uris(workspace_roots);
        let omitted_workspace_roots = workspace_roots
            .len()
            .saturating_sub(MAX_RENDERED_WORKSPACE_ROOTS);
        let workspace_roots = bound_entries(
            workspace_roots
                .iter()
                .map(PathUri::inferred_native_path_string)
                .collect(),
            MAX_RENDERED_WORKSPACE_ROOTS,
        );
        let permission_profile = match permission_profile {
            PermissionProfile::Managed { file_system, .. } => {
                FileSystemPermissionProfileContext::Managed(ManagedFileSystemContext::from(
                    file_system,
                ))
            }
            PermissionProfile::Disabled => FileSystemPermissionProfileContext::Disabled,
            PermissionProfile::External { .. } => FileSystemPermissionProfileContext::External,
        };
        Self {
            workspace_roots,
            omitted_workspace_roots,
            permission_profile,
        }
    }

    pub(super) fn render(&self) -> String {
        let mut rendered = "<filesystem>".to_string();
        if !self.workspace_roots.is_empty() {
            rendered.push_str("<workspace_roots");
            push_truncation_attributes(&mut rendered, self.omitted_workspace_roots);
            rendered.push('>');
            for root in &self.workspace_roots {
                push_text_element(&mut rendered, "root", root);
            }
            rendered.push_str("</workspace_roots>");
        }
        self.permission_profile.render(&mut rendered);
        rendered.push_str("</filesystem>");
        rendered
    }
}

impl From<ManagedFileSystemPermissions> for ManagedFileSystemContext {
    fn from(file_system: ManagedFileSystemPermissions) -> Self {
        match file_system {
            ManagedFileSystemPermissions::Restricted {
                mut entries,
                glob_scan_max_depth,
            } => {
                dedupe_file_system_entries(&mut entries);
                Self::Restricted {
                    entries,
                    glob_scan_max_depth: glob_scan_max_depth.map(usize::from),
                }
            }
            ManagedFileSystemPermissions::Unrestricted => Self::Unrestricted,
        }
    }
}

impl FileSystemPermissionProfileContext {
    fn render(&self, rendered: &mut String) {
        match self {
            Self::Managed(file_system) => {
                rendered.push_str("<permission_profile type=\"managed\">");
                file_system.render(rendered);
                rendered.push_str("</permission_profile>");
            }
            Self::Disabled => {
                rendered.push_str(
                    "<permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile>",
                );
            }
            Self::External => {
                rendered.push_str(
                    "<permission_profile type=\"external\"><file_system type=\"external\" /></permission_profile>",
                );
            }
        }
    }
}

impl ManagedFileSystemContext {
    fn render(&self, rendered: &mut String) {
        match self {
            Self::Restricted {
                entries,
                glob_scan_max_depth,
            } => {
                if entries.is_empty() && glob_scan_max_depth.is_none() {
                    rendered.push_str("<file_system type=\"restricted\" />");
                    return;
                }

                rendered.push_str("<file_system type=\"restricted\"");
                if let Some(glob_scan_max_depth) = glob_scan_max_depth {
                    rendered.push_str(&format!(" glob_scan_max_depth=\"{glob_scan_max_depth}\""));
                }
                rendered.push('>');
                for entry in entries {
                    render_file_system_entry(rendered, entry);
                }
                rendered.push_str("</file_system>");
            }
            Self::Unrestricted => {
                rendered.push_str("<file_system type=\"unrestricted\" />");
            }
        }
    }
}

fn render_file_system_entry(rendered: &mut String, entry: &FileSystemSandboxEntry) {
    rendered.push_str("<entry access=\"");
    let access = entry.access.to_string();
    rendered.push_str(&access);
    if entry.access == FileSystemAccessMode::Deny {
        rendered.push_str("\" escalatable=\"false");
    }
    rendered.push_str("\">");
    match &entry.path {
        FileSystemPath::Path { path } => {
            push_text_element(rendered, "path", &path.inferred_native_path_string());
        }
        FileSystemPath::GlobPattern { pattern } => {
            push_text_element(rendered, "glob", pattern);
        }
        FileSystemPath::Special { value } => {
            let value = render_special_path(value);
            push_text_element(rendered, "special", &value);
        }
    }
    rendered.push_str("</entry>");
}

fn render_special_path(value: &FileSystemSpecialPath) -> String {
    match value {
        FileSystemSpecialPath::Root => ":root".to_string(),
        FileSystemSpecialPath::Minimal => ":minimal".to_string(),
        FileSystemSpecialPath::ProjectRoots { subpath } => {
            render_special_path_with_subpath(":workspace_roots", subpath)
        }
        FileSystemSpecialPath::Tmpdir => ":tmpdir".to_string(),
        FileSystemSpecialPath::SlashTmp => ":slash_tmp".to_string(),
        FileSystemSpecialPath::Unknown { path, subpath } => {
            render_special_path_with_subpath(path, subpath)
        }
    }
}

fn render_special_path_with_subpath(base: &str, subpath: &Option<String>) -> String {
    match subpath {
        Some(subpath) => format!("{base}/{subpath}"),
        None => base.to_string(),
    }
}

fn dedupe_file_system_entries(entries: &mut Vec<FileSystemSandboxEntry>) {
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.clone()));
}

fn push_text_element(rendered: &mut String, name: &str, value: &str) {
    rendered.push_str(&format!("<{name}>"));
    push_xml_escaped_text(rendered, value);
    rendered.push_str(&format!("</{name}>"));
}

pub(crate) fn push_xml_escaped_text(rendered: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => rendered.push_str("&amp;"),
            '<' => rendered.push_str("&lt;"),
            '>' => rendered.push_str("&gt;"),
            '"' => rendered.push_str("&quot;"),
            '\'' => rendered.push_str("&apos;"),
            _ => rendered.push(ch),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NetworkContext {
    allowed_domains: Vec<String>,
    denied_domains: Vec<String>,
    omitted_allowed_domains: usize,
    omitted_denied_domains: usize,
}

impl NetworkContext {
    pub(crate) fn new(allowed_domains: Vec<String>, denied_domains: Vec<String>) -> Self {
        let omitted_allowed_domains = allowed_domains
            .len()
            .saturating_sub(MAX_RENDERED_NETWORK_DOMAINS);
        let omitted_denied_domains = denied_domains
            .len()
            .saturating_sub(MAX_RENDERED_NETWORK_DOMAINS);
        Self {
            allowed_domains: bound_entries(allowed_domains, MAX_RENDERED_NETWORK_DOMAINS),
            denied_domains: bound_entries(denied_domains, MAX_RENDERED_NETWORK_DOMAINS),
            omitted_allowed_domains,
            omitted_denied_domains,
        }
    }

    pub(super) fn render(&self) -> String {
        let mut rendered = "<network enabled=\"true\">".to_string();
        Self::push_rendered_domain_element(
            &mut rendered,
            "allowed",
            &self.allowed_domains,
            self.omitted_allowed_domains,
        );
        Self::push_rendered_domain_element(
            &mut rendered,
            "denied",
            &self.denied_domains,
            self.omitted_denied_domains,
        );
        rendered.push_str("</network>");
        rendered
    }

    fn push_rendered_domain_element(
        rendered_network: &mut String,
        name: &str,
        domains: &[String],
        omitted_domains: usize,
    ) {
        if domains.is_empty() {
            return;
        }

        rendered_network.push_str(&format!("<{name}"));
        push_truncation_attributes(rendered_network, omitted_domains);
        rendered_network.push('>');
        rendered_network.push_str(&domains.join(","));
        rendered_network.push_str(&format!("</{name}>"));
    }
}

fn push_truncation_attributes(rendered: &mut String, omitted: usize) {
    if omitted > 0 {
        rendered.push_str(&format!(" truncated=\"true\" omitted=\"{omitted}\""));
    }
}
