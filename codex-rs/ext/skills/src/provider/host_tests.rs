use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_core_skills::HostSkillsSnapshot;
use codex_core_skills::loader::SkillRoot;
use codex_core_skills::loader::load_skills_from_roots;
use codex_exec_server::LOCAL_FS;
use codex_protocol::protocol::SkillScope;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tokio::sync::Semaphore;

use crate::provider::SkillProvider;
use crate::provider::SkillReadRequest;

use super::HostSkillProvider;
use super::catalog_from_outcome;

#[tokio::test]
async fn host_catalog_entries_carry_their_render_metadata() -> Result<(), Box<dyn std::error::Error>>
{
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "codex-skills-extension-host-provider-{}-{unique}",
        std::process::id()
    ));
    let skill_path = root.join("demo").join("SKILL.md");
    std::fs::create_dir_all(
        skill_path
            .parent()
            .ok_or("skill path should have a parent")?,
    )?;
    std::fs::write(
        &skill_path,
        "---\nname: demo\ndescription: Demo skill.\n---\n# Demo\n",
    )?;
    let root = AbsolutePathBuf::try_from(std::fs::canonicalize(root)?)?;
    let outcome = load_skills_from_roots(
        [SkillRoot {
            path: root.clone(),
            scope: SkillScope::User,
            file_system: Arc::clone(&LOCAL_FS),
            plugin_identity: None,
            plugin_namespace: None,
            plugin_root: None,
            discovery_mode: Default::default(),
        }],
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(1)),
    )
    .await;

    let catalog = catalog_from_outcome(&outcome);

    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(
        (
            catalog.entries[0].display_path_root(),
            catalog.entries[0].prompt_scope(),
        ),
        (
            Some(root.to_string_lossy().replace('\\', "/").as_str()),
            Some(SkillScope::User),
        )
    );

    std::fs::remove_dir_all(root.as_path())?;
    Ok(())
}

#[tokio::test]
async fn host_provider_reads_shadowed_skill_by_exact_path() -> Result<(), Box<dyn std::error::Error>>
{
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "codex-skills-extension-shadowed-host-provider-{}-{unique}",
        std::process::id()
    ));
    let user_root = parent.join("user");
    let system_root = parent.join("system");
    let user_path = user_root.join("demo/SKILL.md");
    let system_path = system_root.join("demo/SKILL.md");
    std::fs::create_dir_all(user_path.parent().ok_or("user skill parent")?)?;
    std::fs::create_dir_all(system_path.parent().ok_or("system skill parent")?)?;
    std::fs::write(
        &user_path,
        "---\nname: demo\ndescription: User demo.\n---\nUser body.\n",
    )?;
    std::fs::write(
        &system_path,
        "---\nname: demo\ndescription: System demo.\n---\nSystem body.\n",
    )?;
    let user_root = AbsolutePathBuf::try_from(std::fs::canonicalize(user_root)?)?;
    let system_root = AbsolutePathBuf::try_from(std::fs::canonicalize(system_root)?)?;
    let outcome = load_skills_from_roots(
        [
            SkillRoot {
                path: user_root,
                scope: SkillScope::User,
                file_system: Arc::clone(&LOCAL_FS),
                plugin_identity: None,
                plugin_namespace: None,
                plugin_root: None,
                discovery_mode: Default::default(),
            },
            SkillRoot {
                path: system_root,
                scope: SkillScope::System,
                file_system: Arc::clone(&LOCAL_FS),
                plugin_identity: None,
                plugin_namespace: None,
                plugin_root: None,
                discovery_mode: Default::default(),
            },
        ],
        /*plugin_skill_snapshots*/ None,
        Arc::new(Semaphore::new(1)),
    )
    .await;
    let snapshot = Arc::new(HostSkillsSnapshot::new(Arc::new(outcome)));
    let catalog = catalog_from_outcome(snapshot.outcome());
    let shadowed = catalog
        .entries
        .iter()
        .find(|entry| entry.prompt_scope() == Some(SkillScope::System))
        .ok_or("shadowed system skill should remain path-addressable")?;

    let result = HostSkillProvider::new()
        .read(SkillReadRequest {
            authority: shadowed.authority.clone(),
            package: shadowed.id.clone(),
            resource: shadowed.main_prompt.clone(),
            resolved_executor_roots: Vec::new(),
            sandbox: None,
            host_snapshot: Some(snapshot),
            mcp_resources: None,
        })
        .await?;

    assert!(result.contents.contains("System body."));
    std::fs::remove_dir_all(parent)?;
    Ok(())
}
