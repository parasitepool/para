pub(crate) trait ToArgs {
    fn to_args(&self) -> Vec<String>;
}

impl ToArgs for String {
    fn to_args(&self) -> Vec<String> {
        self.as_str().to_args()
    }
}

impl ToArgs for &str {
    fn to_args(&self) -> Vec<String> {
        self.split_whitespace().map(str::to_string).collect()
    }
}

impl<const N: usize> ToArgs for [&str; N] {
    fn to_args(&self) -> Vec<String> {
        self.iter().cloned().map(str::to_string).collect()
    }
}

impl ToArgs for Vec<String> {
    fn to_args(&self) -> Vec<String> {
        self.clone()
    }
}
