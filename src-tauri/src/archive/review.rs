use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone)]
struct Edge {
    item: String,
    candidate: String,
    reason: String,
}

#[derive(Clone)]
struct Group {
    id: String,
    token: String,
    items: Vec<String>,
    edges: Vec<Edge>,
}

pub(super) struct ReviewIndex {
    show_hidden: bool,
    groups: Vec<Group>,
}

pub(super) struct ReviewUndo {
    token: String,
    edges: Vec<Edge>,
    moved: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGroupPage {
    total: usize,
    groups: Vec<ReviewGroupSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewGroupSummary {
    id: String,
    token: String,
    count: usize,
    previews: Vec<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGroupDetail {
    id: String,
    token: String,
    count: usize,
    items: Vec<String>,
    reasons: Vec<String>,
    links: Vec<ReviewLinks>,
}

#[derive(Serialize)]
struct ReviewLinks {
    numbers: Vec<usize>,
    more: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    token: String,
    moved: usize,
    errors: Vec<String>,
    retryable: bool,
}

fn root(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        parents[index] = parents[parents[index]];
        index = parents[index];
    }
    index
}

impl Archive {
    fn pending_review_operation(&self, id: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE json_extract(data,'$.item.id')=?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
    }

    fn build_review_index(&self, show_hidden: bool) -> Result<ReviewIndex> {
        self.check()?;
        let hidden = if show_hidden {
            HashSet::new()
        } else {
            self.likely_generated_items()?
        };
        let mut query = self
            .connection
            .prepare(
                "SELECT r.item,r.candidate,r.reason FROM reviews r
             JOIN items i ON i.id=r.item AND i.state='active'
             JOIN items c ON c.id=r.candidate AND c.state='active'
             ORDER BY r.item,r.candidate,r.reason",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok(Edge {
                    item: row.get(0)?,
                    candidate: row.get(1)?,
                    reason: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::<String>::new();
        let mut positions = HashMap::<String, usize>::new();
        let mut parents = Vec::<usize>::new();
        let mut edges = Vec::new();
        for row in rows {
            let edge = row.map_err(|e| e.to_string())?;
            if hidden.contains(&edge.item) || hidden.contains(&edge.candidate) {
                continue;
            }
            let mut index_for = |id: &str| {
                *positions.entry(id.to_owned()).or_insert_with(|| {
                    let index = ids.len();
                    ids.push(id.to_owned());
                    parents.push(index);
                    index
                })
            };
            let a = index_for(&edge.item);
            let b = index_for(&edge.candidate);
            let a_root = root(&mut parents, a);
            let b_root = root(&mut parents, b);
            parents[b_root] = a_root;
            edges.push(edge);
        }
        let mut components = BTreeMap::<usize, (Vec<String>, Vec<Edge>)>::new();
        for (index, id) in ids.into_iter().enumerate() {
            components
                .entry(root(&mut parents, index))
                .or_default()
                .0
                .push(id);
        }
        for edge in edges {
            let index = positions[&edge.item];
            components
                .entry(root(&mut parents, index))
                .or_default()
                .1
                .push(edge);
        }
        let mut groups = Vec::with_capacity(components.len());
        for (_, (mut items, edges)) in components {
            items.sort_unstable();
            let mut hash = blake3::Hasher::new();
            for edge in &edges {
                for value in [&edge.item, &edge.candidate, &edge.reason] {
                    hash.update(&(value.len() as u64).to_le_bytes());
                    hash.update(value.as_bytes());
                }
            }
            groups.push(Group {
                id: items[0].clone(),
                token: hash.finalize().to_hex().to_string(),
                items,
                edges,
            });
        }
        groups.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        Ok(ReviewIndex {
            show_hidden,
            groups,
        })
    }

    fn ensure_review_index(&self, show_hidden: bool) -> Result<()> {
        if self
            .review_index
            .borrow()
            .as_ref()
            .is_none_or(|index| index.show_hidden != show_hidden)
        {
            let index = self.build_review_index(show_hidden)?;
            *self.review_index.borrow_mut() = Some(index);
        }
        Ok(())
    }

    fn review_group(&self, show_hidden: bool, id: &str, token: &str) -> Result<Group> {
        self.ensure_review_index(show_hidden)?;
        self.review_index
            .borrow()
            .as_ref()
            .and_then(|index| {
                index
                    .groups
                    .iter()
                    .find(|group| group.id == id && group.token == token)
            })
            .cloned()
            .ok_or_else(|| "The matches changed. Refresh and review this group again.".into())
    }

    pub fn review_groups(&self, show_hidden: bool, offset: usize) -> Result<ReviewGroupPage> {
        self.ensure_review_index(show_hidden)?;
        let index = self.review_index.borrow();
        let index = index.as_ref().ok_or("Could not load the review list.")?;
        let mut groups = Vec::new();
        for group in index.groups.iter().skip(offset).take(30) {
            let mut previews = Vec::new();
            for id in group.items.iter().take(2) {
                let item = self.item(id)?;
                previews.push(serde_json::json!({
                    "id": item.id, "originalName": item.original_name, "media": {"kind": item.media.kind}
                }));
            }
            groups.push(ReviewGroupSummary {
                id: group.id.clone(),
                token: group.token.clone(),
                count: group.items.len(),
                previews,
            });
        }
        Ok(ReviewGroupPage {
            total: index.groups.len(),
            groups,
        })
    }

    pub fn review_group_detail(
        &self,
        show_hidden: bool,
        id: &str,
        token: &str,
        offset: usize,
    ) -> Result<ReviewGroupDetail> {
        let group = self.review_group(show_hidden, id, token)?;
        let positions: HashMap<_, _> = group
            .items
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        let mut links: Vec<_> = group
            .items
            .iter()
            .skip(offset)
            .take(8)
            .map(|_| ReviewLinks {
                numbers: Vec::new(),
                more: false,
            })
            .collect();
        let mut reasons = Vec::new();
        for edge in &group.edges {
            if reasons.len() < 5 && !reasons.contains(&edge.reason) {
                reasons.push(edge.reason.clone());
            }
            for (item, other) in [(&edge.item, &edge.candidate), (&edge.candidate, &edge.item)] {
                let position = positions[item.as_str()];
                if let Some(link) = position
                    .checked_sub(offset)
                    .and_then(|index| links.get_mut(index))
                {
                    let number = positions[other.as_str()] + 1;
                    if !link.numbers.contains(&number) {
                        if link.numbers.len() < 8 {
                            link.numbers.push(number);
                        } else {
                            link.more = true;
                        }
                    }
                }
            }
        }
        for link in &mut links {
            link.numbers.sort_unstable();
        }
        Ok(ReviewGroupDetail {
            id: group.id,
            token: group.token,
            count: group.items.len(),
            items: group.items.into_iter().skip(offset).take(8).collect(),
            reasons,
            links,
        })
    }

    pub fn resolve_review_group(
        &mut self,
        show_hidden: bool,
        id: &str,
        token: &str,
        discarded: &[String],
    ) -> Result<ReviewResult> {
        let group = self.review_group(show_hidden, id, token)?;
        let selected: HashSet<_> = discarded.iter().map(String::as_str).collect();
        let members: HashSet<_> = group.items.iter().map(String::as_str).collect();
        if selected.len() != discarded.len() || !selected.is_subset(&members) {
            return Err("The selection changed. Review this group again.".into());
        }
        let mut pending = self
            .connection
            .prepare("SELECT json_extract(data,'$.item.id') FROM operations")
            .map_err(|e| e.to_string())?;
        let operations = pending
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for operation in operations {
            if members.contains(operation.map_err(|e| e.to_string())?.as_str()) {
                return Err("A file operation in this group needs recovery. Reopen the archive before reviewing it.".into());
            }
        }
        drop(pending);
        let mut keepers = HashMap::<&str, &str>::new();
        for edge in &group.edges {
            if selected.contains(edge.item.as_str()) && !selected.contains(edge.candidate.as_str())
            {
                keepers.entry(&edge.item).or_insert(&edge.candidate);
            }
            if selected.contains(edge.candidate.as_str()) && !selected.contains(edge.item.as_str())
            {
                keepers.entry(&edge.candidate).or_insert(&edge.item);
            }
        }
        let mut plans = Vec::new();
        for item in &group.items {
            let current = self.item(item)?;
            if current.state != "active" {
                return Err("The matches changed. Refresh and review this group again.".into());
            }
            if !selected.contains(item.as_str()) {
                continue;
            }
            if !self.companions(item)?.is_empty() {
                return Err(format!(
                    "{} has related files and must be kept together.",
                    current.original_name
                ));
            }
            let keeper = keepers.get(item.as_str()).ok_or_else(|| format!("{} has no direct match among the pictures being kept. Keep a directly matched picture too.", current.original_name))?;
            plans.push((item.clone(), (*keeper).to_owned()));
        }
        let cached = self.review_index.borrow_mut().take();
        let undo_token = Uuid::new_v4().to_string();
        let mut moved = Vec::new();
        let mut errors = Vec::new();
        for (item, keeper) in plans {
            if let Err(error) = self.move_to_undo(&item, &keeper) {
                match self.item(&item) {
                    Ok(current) if current.state == "undo" => moved.push(item),
                    Ok(_) => {}
                    Err(status) => errors.push(format!(
                        "Could not check the file after the error: {status}"
                    )),
                }
                errors.push(error);
                break;
            }
            moved.push(item);
        }
        if errors.is_empty() {
            if let Err(error) = self.transaction(|archive| {
                for edge in &group.edges {
                    archive.dismiss_review(&edge.item, &edge.candidate)?;
                }
                Ok(())
            }) {
                errors.push(error);
            } else if let Some(mut index) = cached {
                index.groups.retain(|entry| entry.id != group.id);
                *self.review_index.borrow_mut() = Some(index);
            }
        }
        if let Err(error) = self.backup() {
            errors.push(format!("Could not update the recovery backup: {error}"));
        }
        let count = moved.len();
        self.review_undo = Some(ReviewUndo {
            token: undo_token.clone(),
            edges: group.edges,
            moved,
        });
        Ok(ReviewResult {
            token: undo_token,
            moved: count,
            errors,
            retryable: true,
        })
    }

    pub fn undo_review_group(&mut self, token: &str) -> Result<ReviewResult> {
        let undo = self
            .review_undo
            .take()
            .ok_or("This review can no longer be undone here.")?;
        if undo.token != token {
            self.review_undo = Some(undo);
            return Err("This review can no longer be undone here.".into());
        }
        if let Err(error) = self.transaction(|archive| {
            for edge in &undo.edges {
                archive.add_review(&edge.item, &edge.candidate, &edge.reason)?;
            }
            Ok(())
        }) {
            self.review_undo = Some(undo);
            return Err(error);
        }
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for id in &undo.moved {
            let pending = match self.pending_review_operation(id) {
                Ok(pending) => pending,
                Err(error) => {
                    remaining.push(id.clone());
                    errors.push(format!(
                        "{id}: could not check for pending operations: {error}"
                    ));
                    continue;
                }
            };
            if pending {
                remaining.push(id.clone());
                errors.push(format!(
                    "{id}: a file operation needs recovery. Reopen the archive before restoring it."
                ));
                continue;
            }
            if let Err(error) = self.restore_file(id) {
                let active = self.item(id).map(|item| item.state == "active");
                let pending = self.pending_review_operation(id);
                match (active, pending) {
                    (Ok(true), Ok(false)) => {}
                    (Ok(_), Ok(_)) => remaining.push(id.clone()),
                    (Err(status), _) | (_, Err(status)) => {
                        remaining.push(id.clone());
                        errors.push(format!(
                            "{id}: could not check the file after the error: {status}"
                        ));
                    }
                }
                errors.push(error);
            }
        }
        if let Err(error) = self.backup() {
            errors.push(format!("Could not update the recovery backup: {error}"));
        }
        let restored = undo.moved.len() - remaining.len();
        let retryable = !remaining.is_empty();
        if !remaining.is_empty() {
            self.review_undo = Some(ReviewUndo {
                moved: remaining,
                ..undo
            });
        }
        Ok(ReviewResult {
            token: token.into(),
            moved: restored,
            errors,
            retryable,
        })
    }
}
