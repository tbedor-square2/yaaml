use std::collections::{BTreeMap, BTreeSet};

use crate::{cosine_similarity, MemoryScope};

#[derive(Debug, Clone, PartialEq)]
pub struct ClusterMemory {
    pub memory_id: i64,
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCluster {
    pub memory_ids: Vec<i64>,
    pub mean_distance: f32,
}

pub fn find_consolidation_clusters(
    memories: &[ClusterMemory],
    distance_threshold: f32,
    min_size: usize,
    max_size: usize,
) -> Vec<MemoryCluster> {
    let mut clusters = Vec::new();
    for partition in partition_memories(memories).values() {
        clusters.extend(dbscan_partition(
            partition,
            distance_threshold,
            min_size,
            max_size,
        ));
    }
    clusters.sort_by(|left, right| {
        right
            .memory_ids
            .len()
            .cmp(&left.memory_ids.len())
            .then_with(|| {
                left.mean_distance
                    .partial_cmp(&right.mean_distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    clusters
}

fn partition_memories(memories: &[ClusterMemory]) -> BTreeMap<String, Vec<ClusterMemory>> {
    let mut partitions: BTreeMap<String, Vec<ClusterMemory>> = BTreeMap::new();
    for memory in memories {
        let key = match memory.scope {
            MemoryScope::Global => "global".to_string(),
            MemoryScope::Project => {
                format!("project:{}", memory.project_id.as_deref().unwrap_or(""))
            }
        };
        partitions.entry(key).or_default().push(memory.clone());
    }
    partitions
}

fn dbscan_partition(
    memories: &[ClusterMemory],
    distance_threshold: f32,
    min_size: usize,
    max_size: usize,
) -> Vec<MemoryCluster> {
    let mut visited = BTreeSet::new();
    let mut assigned = BTreeSet::new();
    let mut clusters = Vec::new();

    for index in 0..memories.len() {
        if visited.contains(&index) {
            continue;
        }
        visited.insert(index);
        let neighbors = neighbors(memories, index, distance_threshold);
        if neighbors.len() < min_size {
            continue;
        }
        let mut cluster_indices = BTreeSet::new();
        expand_cluster(
            memories,
            distance_threshold,
            min_size,
            &mut visited,
            &mut assigned,
            &mut cluster_indices,
            neighbors,
        );
        let mut members = cluster_indices
            .into_iter()
            .map(|member| memories[member].clone())
            .collect::<Vec<_>>();
        if members.len() > max_size {
            members = densest_members(&members, max_size);
        }
        clusters.push(MemoryCluster {
            memory_ids: members.iter().map(|member| member.memory_id).collect(),
            mean_distance: mean_distance(&members),
        });
    }
    clusters
}

fn expand_cluster(
    memories: &[ClusterMemory],
    distance_threshold: f32,
    min_size: usize,
    visited: &mut BTreeSet<usize>,
    assigned: &mut BTreeSet<usize>,
    cluster_indices: &mut BTreeSet<usize>,
    mut seeds: Vec<usize>,
) {
    while let Some(index) = seeds.pop() {
        if !visited.contains(&index) {
            visited.insert(index);
            let next_neighbors = neighbors(memories, index, distance_threshold);
            if next_neighbors.len() >= min_size {
                for neighbor in next_neighbors {
                    if !seeds.contains(&neighbor) {
                        seeds.push(neighbor);
                    }
                }
            }
        }
        if !assigned.contains(&index) {
            assigned.insert(index);
            cluster_indices.insert(index);
        }
    }
}

fn neighbors(memories: &[ClusterMemory], index: usize, distance_threshold: f32) -> Vec<usize> {
    memories
        .iter()
        .enumerate()
        .filter_map(|(candidate_index, candidate)| {
            let distance = cosine_distance(&memories[index].embedding, &candidate.embedding)?;
            (distance <= distance_threshold).then_some(candidate_index)
        })
        .collect()
}

fn densest_members(memories: &[ClusterMemory], max_size: usize) -> Vec<ClusterMemory> {
    let mut scored = memories
        .iter()
        .map(|memory| {
            let others = memories
                .iter()
                .filter(|other| other.memory_id != memory.memory_id)
                .cloned()
                .collect::<Vec<_>>();
            (memory.clone(), mean_distance_to(memory, &others))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.memory_id.cmp(&right.0.memory_id))
    });
    scored
        .into_iter()
        .take(max_size)
        .map(|(memory, _)| memory)
        .collect()
}

fn mean_distance(memories: &[ClusterMemory]) -> f32 {
    if memories.len() < 2 {
        return 0.0;
    }
    let mut total = 0.0;
    let mut pairs = 0;
    for left in 0..memories.len() {
        for right in (left + 1)..memories.len() {
            if let Some(distance) =
                cosine_distance(&memories[left].embedding, &memories[right].embedding)
            {
                total += distance;
                pairs += 1;
            }
        }
    }
    if pairs == 0 {
        0.0
    } else {
        total / pairs as f32
    }
}

fn mean_distance_to(memory: &ClusterMemory, others: &[ClusterMemory]) -> f32 {
    if others.is_empty() {
        return 0.0;
    }
    let distances = others
        .iter()
        .filter_map(|other| cosine_distance(&memory.embedding, &other.embedding))
        .collect::<Vec<_>>();
    if distances.is_empty() {
        0.0
    } else {
        distances.iter().sum::<f32>() / distances.len() as f32
    }
}

fn cosine_distance(left: &[f32], right: &[f32]) -> Option<f32> {
    cosine_similarity(left, right).map(|similarity| 1.0 - similarity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(
        memory_id: i64,
        scope: MemoryScope,
        project_id: Option<&str>,
        x: f32,
    ) -> ClusterMemory {
        ClusterMemory {
            memory_id,
            scope,
            project_id: project_id.map(str::to_string),
            embedding: vec![x, 1.0 - x],
        }
    }

    #[test]
    fn global_and_project_memories_do_not_cluster_together() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Global, None, 1.0),
                memory(2, MemoryScope::Project, Some("/tmp/a"), 1.0),
            ],
            0.01,
            2,
            5,
        );

        assert!(clusters.is_empty());
    }

    #[test]
    fn different_project_memories_do_not_cluster_together() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Project, Some("/tmp/a"), 1.0),
                memory(2, MemoryScope::Project, Some("/tmp/b"), 1.0),
            ],
            0.01,
            2,
            5,
        );

        assert!(clusters.is_empty());
    }

    #[test]
    fn similar_same_project_vectors_cluster() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Project, Some("/tmp/a"), 1.0),
                memory(2, MemoryScope::Project, Some("/tmp/a"), 0.99),
                memory(3, MemoryScope::Project, Some("/tmp/a"), 0.98),
            ],
            0.01,
            2,
            5,
        );

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].memory_ids, vec![1, 2, 3]);
    }

    #[test]
    fn noise_points_are_ignored() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Global, None, 1.0),
                memory(2, MemoryScope::Global, None, 0.99),
                memory(3, MemoryScope::Global, None, 0.1),
            ],
            0.01,
            2,
            5,
        );

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].memory_ids, vec![1, 2]);
    }

    #[test]
    fn clusters_sort_by_size_then_tightness() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Project, Some("/tmp/a"), 1.0),
                memory(2, MemoryScope::Project, Some("/tmp/a"), 0.99),
                memory(3, MemoryScope::Project, Some("/tmp/a"), 0.98),
                memory(4, MemoryScope::Project, Some("/tmp/b"), 0.5),
                memory(5, MemoryScope::Project, Some("/tmp/b"), 0.51),
            ],
            0.05,
            2,
            5,
        );

        assert_eq!(clusters[0].memory_ids.len(), 3);
        assert_eq!(clusters[1].memory_ids.len(), 2);
    }

    #[test]
    fn densest_n_selection_caps_large_clusters() {
        let clusters = find_consolidation_clusters(
            &[
                memory(1, MemoryScope::Global, None, 1.0),
                memory(2, MemoryScope::Global, None, 0.99),
                memory(3, MemoryScope::Global, None, 0.98),
                memory(4, MemoryScope::Global, None, 0.8),
            ],
            0.2,
            2,
            3,
        );

        assert_eq!(clusters[0].memory_ids.len(), 3);
        assert!(!clusters[0].memory_ids.contains(&4));
    }
}
