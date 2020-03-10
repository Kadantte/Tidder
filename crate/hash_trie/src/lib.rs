use memmap::MmapMut;
use std::convert::TryInto;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

mod hash;
use hash::*;

fn u32ize<T>(n: T) -> u32
where
    T: TryInto<u32>,
    T::Error: std::fmt::Debug,
{
    n.try_into().unwrap()
}

unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    ::std::slice::from_raw_parts((p as *const T) as *const u8, ::std::mem::size_of::<T>())
}

pub trait HashTreeStorage {
    type Data;
    fn new(data: Self::Data) -> Self;
    fn get(&self, index: u32) -> &Node;
    fn get_mut(&mut self, index: u32) -> &mut Node;
    fn push(&mut self, node: Node);
    fn len(&self) -> u32;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl HashTreeStorage for Vec<Node> {
    type Data = ();
    fn new(_data: Self::Data) -> Self {
        vec![Node::default()]
    }
    fn get(&self, index: u32) -> &Node {
        &self[index as usize]
    }
    fn get_mut(&mut self, index: u32) -> &mut Node {
        &mut self[index as usize]
    }
    fn push(&mut self, node: Node) {
        self.push(node);
    }
    fn len(&self) -> u32 {
        u32ize(self.len())
    }
}

pub struct FileMap {
    file: File,
    mmap: MmapMut,
}

impl HashTreeStorage for FileMap {
    type Data = String;

    fn new(path: Self::Data) -> Self {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .unwrap();

        if file.metadata().unwrap().len() == 0 {
            file.write_all(&[0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        }

        let mmap = unsafe { MmapMut::map_mut(&file).unwrap() };

        Self { file, mmap }
    }
    fn get(&self, index: u32) -> &Node {
        let slice = unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            std::slice::from_raw_parts(
                self.mmap.as_ref().as_ptr() as *const Node,
                self.mmap.len() / std::mem::size_of::<Node>(),
            )
        };
        &slice[index as usize]
    }

    fn get_mut(&mut self, index: u32) -> &mut Node {
        let slice = unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            std::slice::from_raw_parts_mut(
                self.mmap.as_mut().as_mut_ptr() as *mut Node,
                self.mmap.len() / std::mem::size_of::<Node>(),
            )
        };
        &mut slice[index as usize]
    }

    fn push(&mut self, node: Node) {
        self.file
            .write_all_at(
                unsafe { any_as_u8_slice(&node) },
                self.len() as u64 * std::mem::size_of::<Node>() as u64,
            )
            .unwrap();

        std::mem::replace(&mut self.mmap, unsafe {
            MmapMut::map_mut(&self.file).unwrap()
        });
    }

    fn len(&self) -> u32 {
        u32ize(self.mmap.len() / std::mem::size_of::<Node>())
    }
}

#[derive(Debug, Default, PartialEq)]
#[repr(C)]
pub struct Node {
    zero: u32,
    one: u32,
}

#[derive(Debug, Default)]
pub struct HashTrie<S: HashTreeStorage> {
    haystack: S,
}

impl<S: HashTreeStorage> HashTrie<S> {
    pub fn new(data: S::Data) -> Self {
        Self {
            haystack: S::new(data),
        }
    }

    pub fn insert(&mut self, hash: u64) -> bool {
        let (start_pos, mut index) = self.search(hash);

        if start_pos == 63 {
            return true;
        }

        for bit in HashBits::new_at(hash, start_pos) {
            let new_node = Node::default();

            let new_index = self.haystack.len();
            self.haystack.push(new_node);

            if bit == 0 {
                self.haystack.get_mut(index).zero = new_index;
            } else if bit == 1 {
                self.haystack.get_mut(index).one = new_index;
            }

            index = new_index;
        }

        false
    }

    fn search(&self, needle: u64) -> (u8, u32) {
        let mut current_node = self.haystack.get(0);

        let mut next_index = 0;

        for (pos, bit) in HashBits::new(needle).enumerate() {
            next_index = if bit == 0 && current_node.zero != 0 {
                current_node.zero
            } else if bit == 1 && current_node.one != 0 {
                current_node.one
            } else {
                return (pos as u8, next_index);
            };

            current_node = self.haystack.get(next_index);
        }

        (63, next_index)
    }

    pub fn similar(&self, needle: u64, max_distance: u8) -> Similar<S> {
        Similar::new(self, needle, max_distance)
    }

    pub fn hashes(&self) -> HashIter<S> {
        HashIter::new(self)
    }
}

impl HashTrie<Vec<Node>> {
    pub fn read_in(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;

        let len = file.metadata()?.len();

        let mut file = BufReader::new(file);

        let mut new = Self {
            haystack: Vec::new(),
        };

        for _i in 0..len / (2 * std::mem::size_of::<u32>() as u64) {
            let mut zero_bytes = [0, 0, 0, 0];
            let mut one_bytes = [0, 0, 0, 0];

            file.read_exact(&mut zero_bytes)?;
            file.read_exact(&mut one_bytes)?;

            let zero = u32::from_le_bytes(zero_bytes);
            let one = u32::from_le_bytes(one_bytes);

            new.haystack.push(Node { zero, one });
        }

        Ok(new)
    }

    pub fn write_out(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let mut file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)?,
        );

        for node in self.haystack.iter() {
            file.write_all(&node.zero.to_le_bytes())?;
            file.write_all(&node.one.to_le_bytes())?;
        }

        file.flush()
    }
}

impl std::iter::FromIterator<u64> for HashTrie<Vec<Node>> {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = u64>,
    {
        let mut new = Self::new(());

        for hash in iter {
            new.insert(hash);
        }

        new
    }
}

struct SimilarBranch<'a> {
    hash: u64,
    pos: u8,
    distance: u8,
    node: &'a Node,
}

pub struct Similar<'a, S: HashTreeStorage> {
    trie: &'a HashTrie<S>,
    needle: u64,
    max_distance: u8,
    branches: Vec<SimilarBranch<'a>>,
}

impl<'a, S: HashTreeStorage> Similar<'a, S> {
    fn new(trie: &'a HashTrie<S>, needle: u64, max_distance: u8) -> Self {
        Self {
            trie,
            needle,
            max_distance,
            branches: vec![SimilarBranch {
                hash: 0,
                pos: 0,
                distance: 0,
                node: &trie.haystack.get(0),
            }],
        }
    }
}

impl<'a, S: HashTreeStorage> Iterator for Similar<'a, S> {
    type Item = u64;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(SimilarBranch {
            mut hash,
            mut distance,
            pos: start_pos,
            mut node,
        }) = self.branches.pop()
        {
            for pos in start_pos..=64 {
                let index = match (node.zero, node.one) {
                    (0, 0) => {
                        debug_assert_eq!(pos, 64);
                        return Some(hash);
                    }
                    (index, 0) => {
                        if get_bit(self.needle, pos) == 0 {
                            index
                        } else {
                            distance += 1;
                            if distance <= self.max_distance {
                                index
                            } else {
                                break;
                            }
                        }
                    }
                    (0, index) => {
                        hash |= 1 << pos;

                        if get_bit(self.needle, pos) == 1 {
                            index
                        } else {
                            distance += 1;
                            if distance <= self.max_distance {
                                index
                            } else {
                                break;
                            }
                        }
                    }
                    (zero_index, one_index) => {
                        let needle_bit = get_bit(self.needle, pos);

                        if needle_bit == 1 || distance < self.max_distance {
                            let branch_distance = if needle_bit == 1 {
                                distance
                            } else {
                                distance + 1
                            };

                            self.branches.push(SimilarBranch {
                                hash: hash | 1 << pos,
                                pos: pos + 1,
                                distance: branch_distance,
                                node: &self.trie.haystack.get(one_index),
                            });
                        }

                        if needle_bit == 0 {
                            zero_index
                        } else {
                            distance += 1;
                            if distance <= self.max_distance {
                                zero_index
                            } else {
                                break;
                            }
                        }
                    }
                };
                debug_assert_ne!(pos, 64);
                node = &self.trie.haystack.get(index);
            }
        }

        None
    }
}

pub struct HashIter<'a, S: HashTreeStorage> {
    trie: &'a HashTrie<S>,
    branches: Vec<(u64, u8, &'a Node)>,
}

impl<'a, S: HashTreeStorage> HashIter<'a, S> {
    fn new(trie: &'a HashTrie<S>) -> Self {
        Self {
            trie,
            branches: vec![(0, 0, &trie.haystack.get(0))],
        }
    }
}

impl<'a, S: HashTreeStorage> Iterator for HashIter<'a, S> {
    type Item = u64;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((mut hash, start_pos, mut node)) = self.branches.pop() {
            for pos in start_pos..64 {
                let index = match (node.zero, node.one) {
                    (0, 0) => unreachable!(),
                    (index, 0) => index,
                    (0, index) => {
                        hash |= 1 << pos;
                        index
                    }
                    (zero_index, one_index) => {
                        self.branches.push((
                            hash | 1 << pos,
                            pos + 1,
                            &self.trie.haystack.get(one_index),
                        ));
                        zero_index
                    }
                };
                debug_assert_ne!(pos, 64);
                node = &self.trie.haystack.get(index);
            }

            Some(hash)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use rand::prelude::*;

    #[test]
    fn inout() {
        let mut input = vec![1, 54, 0, std::u64::MAX, 766];

        let trie: HashTrie<Vec<_>> = input.iter().copied().collect();
        let mut output = trie.hashes().collect::<Vec<_>>();

        input.sort();
        output.sort();

        assert_eq!(input, output);
    }

    #[test]
    fn random_inout() {
        let mut rng = thread_rng();

        let mut input: Vec<_> = std::iter::repeat_with(|| rng.gen()).take(1000).collect();

        let trie: HashTrie<Vec<_>> = input.iter().copied().collect();
        let mut output: Vec<_> = trie.hashes().collect();

        input.sort();
        output.sort();

        assert_eq!(input, output);
    }

    #[test]
    fn similar() {
        let input = [
            0b1001, 0b0100, 0b0010, 0b0101, 0b0110, 0b0001, 0b0000, 0b1111, 0b0011,
        ];

        let trie: HashTrie<Vec<_>> = input.iter().copied().collect();

        let needle = 0b0010;
        let max_distance = 1;
        let mut should_match = vec![0b0000, 0b0011, 0b0010, 0b0110];
        should_match.sort();

        let mut matches: Vec<_> = trie.similar(needle, max_distance).collect();
        matches.sort();

        assert_eq!(should_match, matches);
    }

    #[test]
    fn save() {
        let mut rng = thread_rng();

        let input: Vec<u64> = std::iter::repeat_with(|| rng.gen()).take(1).collect();

        let in_trie: HashTrie<Vec<_>> = input.iter().copied().collect();

        in_trie.write_out("/tmp/test.hashtrie").unwrap();

        let out_trie = HashTrie::read_in("/tmp/test.hashtrie").unwrap();

        assert_eq!(in_trie.haystack, out_trie.haystack);
    }

    #[test]
    fn mmap() {
        if std::path::Path::exists("/tmp/test.mmaptrie".as_ref()) {
            std::fs::remove_file("/tmp/test.mmaptrie").unwrap();
        }

        let mut rng = thread_rng();

        let mut input: Vec<u64> = std::iter::repeat_with(|| rng.gen()).take(100).collect();
        input.sort();

        let mut trie = HashTrie::<FileMap>::new("/tmp/test.mmaptrie".to_string());

        for hash in input.iter() {
            trie.insert(*hash);
        }

        let mut output = trie.hashes().collect::<Vec<_>>();
        output.sort();

        assert_eq!(input, output);
    }

    #[test]
    fn both() {}
}
