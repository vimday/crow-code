// crates/crow-intel/src/pagerank.rs
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub type NodePath = PathBuf;
pub type SymbolName = String;
pub type SymbolMap = HashMap<NodePath, HashSet<SymbolName>>;
pub type ContentMap = HashMap<NodePath, String>;
pub type RankMap<'a> = HashMap<&'a NodePath, f64>;
pub type AdjacencyGraph<'a> = HashMap<&'a NodePath, HashSet<&'a NodePath>>;

/// Computes a personalized PageRank for files in a repository.
///
/// `file_definitions`: maps a File Path -> Set of symbol names defined in that file.
/// `file_contents`: maps a File Path -> Raw string content of the file.
/// `active_files`: Set of File Paths that the user is currently editing or mentioning (Personalization vector).
///
/// Returns a sorted list of (PathBuf, rank_score).
pub fn compute_personalized_pagerank(
    file_definitions: &SymbolMap,
    file_contents: &ContentMap,
    active_files: &HashSet<NodePath>,
) -> Vec<(NodePath, f64)> {
    let mut pages: Vec<PathBuf> = file_contents.keys().cloned().collect();
    pages.sort(); // deterministic ordering
    let n = pages.len();
    if n == 0 {
        return Vec::new();
    }

    // Build bipartite edges: File -> File
    let mut adjacency: AdjacencyGraph = HashMap::new();
    for page in &pages {
        adjacency.insert(page, HashSet::new());
    }

    // Invert definitions: Symbol -> Vec<Defining Files>
    let mut symbol_to_files: HashMap<&str, Vec<&PathBuf>> = HashMap::new();
    for (file, defs) in file_definitions {
        for def in defs {
            if def.len() > 3 {
                symbol_to_files.entry(def).or_default().push(file);
            }
        }
    }

    // Connect files
    for (file_a, content) in file_contents {
        for (symbol, defining_files) in &symbol_to_files {
            if content.contains(*symbol) {
                for file_b in defining_files {
                    if file_a != *file_b {
                        if let Some(set) = adjacency.get_mut(file_a) {
                            set.insert(*file_b);
                        }
                        if let Some(set) = adjacency.get_mut(*file_b) {
                            set.insert(file_a);
                        }
                    }
                }
            }
        }
    }

    // Initialize PageRank
    let mut ranks: RankMap = HashMap::new();
    let initial_rank = 1.0 / (n as f64);
    for page in &pages {
        ranks.insert(page, initial_rank);
    }

    // Personalization vector
    let mut p_vec: RankMap = HashMap::new();
    let num_active = active_files.len();
    if num_active > 0 {
        let active_weight = 1.0 / (num_active as f64);
        for page in &pages {
            if active_files.contains(page) {
                p_vec.insert(page, active_weight);
            } else {
                p_vec.insert(page, 0.0);
            }
        }
    } else {
        // Default to uniform
        for page in &pages {
            p_vec.insert(page, initial_rank);
        }
    }

    let damping = 0.85;
    let num_iterations = 20;

    for _ in 0..num_iterations {
        let mut new_ranks: RankMap = HashMap::new();
        for page in &pages {
            new_ranks.insert(page, (1.0 - damping) * p_vec[page]);
        }

        for (u, neighbors) in &adjacency {
            if neighbors.is_empty() {
                if let (Some(&rank_u), Some(_)) = (ranks.get(u), p_vec.get(u)) {
                    let dangling_share = rank_u * damping;
                    for page in &pages {
                        if let (Some(nr), Some(&p_v)) = (new_ranks.get_mut(page), p_vec.get(page)) {
                            *nr += dangling_share * p_v;
                        }
                    }
                }
            } else if let Some(&rank_u) = ranks.get(u) {
                let share = (rank_u * damping) / (neighbors.len() as f64);
                for v in neighbors {
                    if let Some(nr) = new_ranks.get_mut(v) {
                        *nr += share;
                    }
                }
            }
        }
        ranks = new_ranks;
    }

    let mut result: Vec<(PathBuf, f64)> = ranks.into_iter().map(|(p, r)| (p.clone(), r)).collect();
    result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    result
}
