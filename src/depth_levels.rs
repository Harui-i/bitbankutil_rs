//! 更新した小さな区画だけをコピーする板。公開Depth APIのBTreeMapは要求時に構築する。
use crate::book_units::{Levels, Price, Quantity};
use std::sync::{Arc, OnceLock};

// 少量の価格更新で大きな区画をコピーしないよう、共有単位を小さく保つ。
const CHUNK: usize = 8;
type Chunk = Arc<Vec<(Price, Quantity)>>;

#[derive(Debug, Default)]
pub(crate) struct DepthLevels {
    chunks: Vec<Chunk>,
    materialized: OnceLock<Levels>,
}
impl Clone for DepthLevels {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            materialized: OnceLock::new(),
        }
    }
}
impl PartialEq for DepthLevels {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl From<Levels> for DepthLevels {
    fn from(levels: Levels) -> Self {
        let mut result = Self::default();
        let mut chunk = Vec::with_capacity(CHUNK);
        for entry in levels {
            chunk.push(entry);
            if chunk.len() == CHUNK {
                result.chunks.push(Arc::new(std::mem::take(&mut chunk)));
            }
        }
        if !chunk.is_empty() {
            result.chunks.push(Arc::new(chunk));
        }
        result
    }
}
impl DepthLevels {
    pub fn materialize(&self) -> &Levels {
        self.materialized
            .get_or_init(|| self.iter().map(|(&p, &q)| (p, q)).collect())
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Price, &Quantity)> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter().map(|(p, q)| (p, q)))
    }
    pub fn first_key_value(&self) -> Option<(&Price, &Quantity)> {
        self.chunks.first()?.first().map(|(p, q)| (p, q))
    }
    pub fn last_key_value(&self) -> Option<(&Price, &Quantity)> {
        self.chunks.last()?.last().map(|(p, q)| (p, q))
    }
    fn chunk_index(&self, price: &Price) -> usize {
        self.chunks
            .partition_point(|c| c.last().unwrap().0 < *price)
            .min(self.chunks.len().saturating_sub(1))
    }
    pub fn get(&self, price: &Price) -> Option<&Quantity> {
        let chunk = self.chunks.get(self.chunk_index(price))?;
        chunk
            .binary_search_by_key(price, |(p, _)| *p)
            .ok()
            .map(|i| &chunk[i].1)
    }
    pub fn insert(&mut self, price: Price, amount: Quantity) {
        self.materialized.take();
        if self.chunks.is_empty() {
            self.chunks.push(Arc::new(vec![(price, amount)]));
            return;
        }
        let i = self.chunk_index(&price);
        let chunk = Arc::make_mut(&mut self.chunks[i]);
        match chunk.binary_search_by_key(&price, |(p, _)| *p) {
            Ok(at) => chunk[at].1 = amount,
            Err(at) => chunk.insert(at, (price, amount)),
        }
        if chunk.len() > CHUNK * 2 {
            let right = chunk.split_off(CHUNK);
            self.chunks.insert(i + 1, Arc::new(right));
        }
    }
    pub fn remove(&mut self, price: &Price) {
        let i = self.chunk_index(price);
        let Some(chunk) = self.chunks.get(i) else {
            return;
        };
        let Ok(at) = chunk.binary_search_by_key(price, |(p, _)| *p) else {
            return;
        };
        self.materialized.take();
        let chunk = Arc::make_mut(&mut self.chunks[i]);
        chunk.remove(at);
        if chunk.is_empty() {
            self.chunks.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_and_order_match_btree_through_insert_delete_and_split() {
        let mut book = DepthLevels::default();
        let mut reference = Levels::new();
        let mut snapshots = Vec::new();
        let mut random = 12345_u64;
        for step in 0..5000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let price = Price::from(((random >> 32) % 400) as i32);
            if random & 3 == 0 {
                book.remove(&price);
                reference.remove(&price);
            } else {
                let amount = Quantity::from(step);
                book.insert(price, amount);
                reference.insert(price, amount);
            }
            assert_eq!(book.get(&price), reference.get(&price));
            assert_eq!(book.first_key_value(), reference.first_key_value());
            assert_eq!(book.last_key_value(), reference.last_key_value());
            assert_eq!(book.materialize(), &reference);
            if step % 100 == 0 {
                snapshots.push((book.clone(), reference.clone()));
            }
        }
        for (book, reference) in snapshots {
            assert_eq!(book.materialize(), &reference);
        }
        for price in reference.keys() {
            book.remove(price);
        }
        assert!(book.first_key_value().is_none());
        assert!(book.last_key_value().is_none());
    }
}
