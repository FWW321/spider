use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum NoneOrOne<T> {
    #[serde(skip_deserializing)]
    #[default]
    Unspecified,
    None,
    One(T),
}

impl<T> NoneOrOne<T> {
    pub fn is_unspecified(&self) -> bool {
        matches!(self, NoneOrOne::Unspecified)
    }

    pub fn into_option(self) -> Option<T> {
        match self {
            NoneOrOne::One(item) => Some(item),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_one(&self) -> bool {
        matches!(self, NoneOrOne::One(_))
    }
}

#[derive(Default, Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum NoneOrSome<T> {
    #[serde(skip_deserializing)]
    #[default]
    Unspecified,
    None,
    One(T),
    Some(Vec<T>),
}

impl<T> NoneOrSome<T> {
    pub fn is_unspecified(&self) -> bool {
        matches!(self, NoneOrSome::Unspecified)
    }

    pub fn len(&self) -> usize {
        match self {
            NoneOrSome::Unspecified => 0,
            NoneOrSome::None => 0,
            NoneOrSome::One(_) => 1,
            NoneOrSome::Some(v) => v.len(),
        }
    }

    pub fn into_vec(self) -> Vec<T> {
        match self {
            NoneOrSome::Unspecified | NoneOrSome::None => vec![],
            NoneOrSome::One(item) => vec![item],
            NoneOrSome::Some(v) => v,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            NoneOrSome::Unspecified => true,
            NoneOrSome::None => true,
            NoneOrSome::One(_) => false,
            NoneOrSome::Some(v) => v.is_empty(),
        }
    }

    pub fn map<F, U>(self, mut f: F) -> NoneOrSome<U>
    where
        F: FnMut(T) -> U,
    {
        match self {
            NoneOrSome::Unspecified => NoneOrSome::Unspecified,
            NoneOrSome::None => NoneOrSome::None,
            NoneOrSome::One(item) => NoneOrSome::One(f(item)),
            NoneOrSome::Some(v) => NoneOrSome::Some(v.into_iter().map(f).collect()),
        }
    }

    pub fn iter(&self) -> NoneOrSomeRefIter<'_, T> {
        match self {
            NoneOrSome::Unspecified | NoneOrSome::None => NoneOrSomeRefIter::None,
            NoneOrSome::One(item) => NoneOrSomeRefIter::One(Some(item)),
            NoneOrSome::Some(v) => NoneOrSomeRefIter::Some(v.iter()),
        }
    }

    pub fn iter_mut(&mut self) -> NoneOrSomeRefIterMut<'_, T> {
        match self {
            NoneOrSome::Unspecified | NoneOrSome::None => NoneOrSomeRefIterMut::None,
            NoneOrSome::One(item) => NoneOrSomeRefIterMut::One(Some(item)),
            NoneOrSome::Some(v) => NoneOrSomeRefIterMut::Some(v.iter_mut()),
        }
    }

    pub fn into_iter(self) -> NoneOrSomeIter<T> {
        match self {
            NoneOrSome::Unspecified | NoneOrSome::None => NoneOrSomeIter::None,
            NoneOrSome::One(item) => NoneOrSomeIter::One(Some(item)),
            NoneOrSome::Some(v) => NoneOrSomeIter::Some(v.into_iter()),
        }
    }
}

pub enum NoneOrSomeRefIter<'a, T> {
    None,
    One(Option<&'a T>),
    Some(std::slice::Iter<'a, T>),
}

impl<'a, T> Iterator for NoneOrSomeRefIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            NoneOrSomeRefIter::None => None,
            NoneOrSomeRefIter::One(opt) => opt.take(),
            NoneOrSomeRefIter::Some(iter) => iter.next(),
        }
    }
}

pub enum NoneOrSomeRefIterMut<'a, T> {
    None,
    One(Option<&'a mut T>),
    Some(std::slice::IterMut<'a, T>),
}

impl<'a, T> Iterator for NoneOrSomeRefIterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            NoneOrSomeRefIterMut::None => None,
            NoneOrSomeRefIterMut::One(opt) => opt.take(),
            NoneOrSomeRefIterMut::Some(iter) => iter.next(),
        }
    }
}

pub enum NoneOrSomeIter<T> {
    None,
    One(Option<T>),
    Some(std::vec::IntoIter<T>),
}

impl<T> Iterator for NoneOrSomeIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            NoneOrSomeIter::None => None,
            NoneOrSomeIter::One(opt) => opt.take(),
            NoneOrSomeIter::Some(iter) => iter.next(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum OneOrSome<T> {
    One(T),
    #[serde(deserialize_with = "validate_non_empty")]
    Some(Vec<T>),
}

fn validate_non_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::de::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = Vec::deserialize(d)?;
    if value.is_empty() {
        return Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Other("empty"),
            &"need at least one element",
        ));
    }
    Ok(value)
}

impl<T> OneOrSome<T> {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        match self {
            OneOrSome::One(_) => 1,
            OneOrSome::Some(v) => v.len(),
        }
    }

    pub fn into_vec(self) -> Vec<T> {
        match self {
            OneOrSome::One(item) => vec![item],
            OneOrSome::Some(v) => v,
        }
    }

    pub fn iter(&self) -> OneOrSomeRefIter<'_, T> {
        match self {
            OneOrSome::One(item) => OneOrSomeRefIter::One(Some(item)),
            OneOrSome::Some(v) => OneOrSomeRefIter::Some(v.iter()),
        }
    }

    pub fn iter_mut(&mut self) -> OneOrSomeRefIterMut<'_, T> {
        match self {
            OneOrSome::One(item) => OneOrSomeRefIterMut::One(Some(item)),
            OneOrSome::Some(v) => OneOrSomeRefIterMut::Some(v.iter_mut()),
        }
    }

    pub fn _contains(&self, x: &T) -> bool
    where
        T: PartialEq,
    {
        match self {
            OneOrSome::One(item) => item == x,
            OneOrSome::Some(v) => v.contains(x),
        }
    }

    pub fn into_iter(self) -> OneOrSomeIter<T> {
        match self {
            OneOrSome::One(item) => OneOrSomeIter::One(Some(item)),
            OneOrSome::Some(v) => OneOrSomeIter::Some(v.into_iter()),
        }
    }
}

pub enum OneOrSomeRefIter<'a, T> {
    One(Option<&'a T>),
    Some(std::slice::Iter<'a, T>),
}

impl<'a, T> Iterator for OneOrSomeRefIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrSomeRefIter::One(opt) => opt.take(),
            OneOrSomeRefIter::Some(iter) => iter.next(),
        }
    }
}

pub enum OneOrSomeRefIterMut<'a, T> {
    One(Option<&'a mut T>),
    Some(std::slice::IterMut<'a, T>),
}

impl<'a, T> Iterator for OneOrSomeRefIterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrSomeRefIterMut::One(opt) => opt.take(),
            OneOrSomeRefIterMut::Some(iter) => iter.next(),
        }
    }
}

pub enum OneOrSomeIter<T> {
    One(Option<T>),
    Some(std::vec::IntoIter<T>),
}

impl<T> Iterator for OneOrSomeIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrSomeIter::One(opt) => opt.take(),
            OneOrSomeIter::Some(iter) => iter.next(),
        }
    }
}

impl<T> TryFrom<Vec<T>> for OneOrSome<T> {
    type Error = std::io::Error;
    fn try_from(vec: Vec<T>) -> std::io::Result<Self> {
        match vec.len() {
            0 => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Cannot create OneOrSome from empty vector",
            )),
            1 => Ok(OneOrSome::One(vec.into_iter().next().unwrap())),
            _ => Ok(OneOrSome::Some(vec)),
        }
    }
}
