//! Layout of overlapping events: side-by-side columns in the time grid,
//! stacked lanes for all-day events in a week row.

/// Where an event of a day column goes: column `column` of `columns`
/// equal columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub column: usize,
    pub columns: usize,
}

/// Columns for intervals `[start, end)` of one day. Overlapping events
/// form a cluster (transitively); each event takes the first free column,
/// and all events of a cluster share its number of columns. Empty
/// intervals count as `min_length` long, so they stay visible.
pub fn columns(spans: &[(i64, i64)], min_length: i64) -> Vec<Placed> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    let end = |i: usize| spans[i].1.max(spans[i].0 + min_length);
    order.sort_by_key(|&i| (spans[i].0, std::cmp::Reverse(end(i))));
    let mut placed = vec![
        Placed {
            column: 0,
            columns: 1
        };
        spans.len()
    ];
    let mut cluster: Vec<usize> = Vec::new();
    // End of the last event in each column of the current cluster.
    let mut column_ends: Vec<i64> = Vec::new();
    let mut cluster_end = i64::MIN;
    let finish = |cluster: &mut Vec<usize>, columns: usize, placed: &mut Vec<Placed>| {
        for &i in cluster.iter() {
            placed[i].columns = columns;
        }
        cluster.clear();
    };
    for &i in &order {
        let (start, stop) = (spans[i].0, end(i));
        if start >= cluster_end && !cluster.is_empty() {
            finish(&mut cluster, column_ends.len(), &mut placed);
            column_ends.clear();
        }
        let column = match column_ends.iter().position(|&e| e <= start) {
            Some(column) => {
                column_ends[column] = stop;
                column
            }
            None => {
                column_ends.push(stop);
                column_ends.len() - 1
            }
        };
        placed[i].column = column;
        cluster.push(i);
        cluster_end = if cluster.len() == 1 {
            stop
        } else {
            cluster_end.max(stop)
        };
    }
    finish(&mut cluster, column_ends.len().max(1), &mut placed);
    placed
}

/// Lanes (rows from the top) for day ranges `[first, last]` (inclusive
/// day indices of one week row): each takes the first lane free on all its
/// days.
pub fn lanes(ranges: &[(i64, i64)]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    // Longer spans first, so they get the top lanes.
    order.sort_by_key(|&i| (ranges[i].0, std::cmp::Reverse(ranges[i].1)));
    let mut lane_ends: Vec<i64> = Vec::new();
    let mut lanes = vec![0; ranges.len()];
    for i in order {
        let (first, last) = ranges[i];
        let lane = match lane_ends.iter().position(|&end| end < first) {
            Some(lane) => {
                lane_ends[lane] = last;
                lane
            }
            None => {
                lane_ends.push(last);
                lane_ends.len() - 1
            }
        };
        lanes[i] = lane;
    }
    lanes
}
