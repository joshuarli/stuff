#![forbid(unsafe_code)]

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    variables: Vec<(String, String)>,
}

impl Environment {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            variables: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.variables
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn non_empty(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|value| !value.is_empty())
    }

    pub fn variables(&self) -> impl Iterator<Item = (&str, &str)> {
        self.variables
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}
