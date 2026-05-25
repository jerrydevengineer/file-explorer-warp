use std::path::Path;

fn open_repo(workdir: &Path) -> Result<git2::Repository, String> {
    git2::Repository::discover(workdir).map_err(|e| e.message().to_string())
}

pub fn stage_file(workdir: &Path, path: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let mut index = repo.index().map_err(|e| e.message().to_string())?;
    index
        .add_path(Path::new(path))
        .map_err(|e| e.message().to_string())?;
    index.write().map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn unstage_file(workdir: &Path, path: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let head = repo
        .head()
        .map_err(|e| e.message().to_string())?
        .peel_to_commit()
        .map_err(|e| e.message().to_string())?;
    repo.reset_default(Some(head.as_object()), [path])
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn stage_all(workdir: &Path) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let mut index = repo.index().map_err(|e| e.message().to_string())?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| e.message().to_string())?;
    index.write().map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn unstage_all(workdir: &Path) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let head = repo
        .head()
        .map_err(|e| e.message().to_string())?
        .peel_to_commit()
        .map_err(|e| e.message().to_string())?;
    repo.reset(head.as_object(), git2::ResetType::Mixed, None)
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn commit(workdir: &Path, message: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let sig = repo.signature().map_err(|e| e.message().to_string())?;
    let tree_oid = repo
        .index()
        .map_err(|e| e.message().to_string())?
        .write_tree()
        .map_err(|e| e.message().to_string())?;
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| e.message().to_string())?;
    let parents: Vec<git2::Commit> = match repo.head() {
        Ok(head) => vec![head
            .peel_to_commit()
            .map_err(|e| e.message().to_string())?],
        Err(_) => vec![], // initial commit
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn checkout_branch(workdir: &Path, name: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|e| e.message().to_string())?;
    let obj = branch
        .get()
        .peel(git2::ObjectType::Commit)
        .map_err(|e| e.message().to_string())?;
    repo.checkout_tree(&obj, None)
        .map_err(|e| e.message().to_string())?;
    repo.set_head(&format!("refs/heads/{}", name))
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn create_branch(workdir: &Path, name: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let head = repo
        .head()
        .map_err(|e| e.message().to_string())?
        .peel_to_commit()
        .map_err(|e| e.message().to_string())?;
    repo.branch(name, &head, false)
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn delete_branch(workdir: &Path, name: &str) -> Result<(), String> {
    let repo = open_repo(workdir)?;
    let mut branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|e| e.message().to_string())?;
    branch.delete().map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn stash_save(workdir: &Path) -> Result<(), String> {
    let mut repo = open_repo(workdir)?;
    let sig = repo.signature().map_err(|e| e.message().to_string())?;
    repo.stash_save(&sig, "WIP", Some(git2::StashFlags::DEFAULT))
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn stash_apply(workdir: &Path, index: usize) -> Result<(), String> {
    let mut repo = open_repo(workdir)?;
    repo.stash_apply(index, None)
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

pub fn stash_drop(workdir: &Path, index: usize) -> Result<(), String> {
    let mut repo = open_repo(workdir)?;
    repo.stash_drop(index)
        .map_err(|e| e.message().to_string())?;
    Ok(())
}

fn make_callbacks<'a>() -> git2::RemoteCallbacks<'a> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, allowed_types| {
        if allowed_types.contains(git2::CredentialType::SSH_KEY) {
            return git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"));
        }
        if allowed_types.contains(git2::CredentialType::DEFAULT) {
            return git2::Cred::default();
        }
        Err(git2::Error::from_str("no suitable auth"))
    });
    callbacks
}

pub fn fetch(workdir: &Path, remote: &str) -> Result<String, String> {
    let repo = open_repo(workdir)?;
    let callbacks = make_callbacks();
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    let mut remote_obj = repo
        .find_remote(remote)
        .map_err(|e| e.message().to_string())?;
    remote_obj
        .fetch(&[] as &[&str], Some(&mut fetch_opts), None)
        .map_err(|e| e.message().to_string())?;
    Ok(format!("Fetched from {}", remote))
}

pub fn pull(workdir: &Path, remote: &str) -> Result<String, String> {
    let repo = open_repo(workdir)?;

    let branch_name = repo
        .head()
        .map_err(|e| e.message().to_string())?
        .shorthand()
        .ok_or_else(|| "Could not determine current branch".to_string())?
        .to_string();

    // Fetch
    let callbacks = make_callbacks();
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    let mut remote_obj = repo
        .find_remote(remote)
        .map_err(|e| e.message().to_string())?;
    remote_obj
        .fetch(&[branch_name.as_str()], Some(&mut fetch_opts), None)
        .map_err(|e| e.message().to_string())?;
    drop(remote_obj);

    // Fast-forward
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|e| e.message().to_string())?;
    let fetch_commit = repo
        .reference_to_annotated_commit(&fetch_head)
        .map_err(|e| e.message().to_string())?;
    let (analysis, _) = repo
        .merge_analysis(&[&fetch_commit])
        .map_err(|e| e.message().to_string())?;

    if analysis.is_fast_forward() {
        let refname = format!("refs/heads/{}", branch_name);
        if let Ok(mut reference) = repo.find_reference(&refname) {
            reference
                .set_target(fetch_commit.id(), "Fast-forward")
                .map_err(|e| e.message().to_string())?;
            repo.set_head(&refname)
                .map_err(|e| e.message().to_string())?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
                .map_err(|e| e.message().to_string())?;
            return Ok(format!("Fast-forwarded {} from {}", branch_name, remote));
        }
    } else if analysis.is_up_to_date() {
        return Ok("Already up to date.".to_string());
    } else {
        return Err("Cannot fast-forward; manual merge required.".to_string());
    }

    Ok(format!("Pulled {} from {}", branch_name, remote))
}

pub fn push_branch(workdir: &Path, remote: &str, branch: &str) -> Result<String, String> {
    let repo = open_repo(workdir)?;
    let callbacks = make_callbacks();
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(callbacks);
    let mut remote_obj = repo
        .find_remote(remote)
        .map_err(|e| e.message().to_string())?;
    let refspec = format!("refs/heads/{}:refs/heads/{}", branch, branch);
    remote_obj
        .push(&[refspec.as_str()], Some(&mut push_opts))
        .map_err(|e| e.message().to_string())?;
    Ok(format!("Pushed {} to {}", branch, remote))
}
