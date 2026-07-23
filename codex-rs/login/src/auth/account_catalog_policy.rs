#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginAccountCatalogPolicy {
    Mirror,
    Isolated,
}

impl LoginAccountCatalogPolicy {
    pub(crate) fn should_mirror(self) -> bool {
        self == Self::Mirror
    }
}
