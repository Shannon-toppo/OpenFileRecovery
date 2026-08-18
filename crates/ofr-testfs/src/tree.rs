//! イメージに書き込む中身の指定。

/// ファイル / ディレクトリ 1 つ。
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    pub data: Vec<u8>,
    /// 削除済みとして書く(エントリに削除マークを付け、割り当てを解放する)。
    pub deleted: bool,
    /// 断片化させて配置する。
    pub fragmented: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

/// イメージに入れるツリー。index 0 がルート。
#[derive(Debug, Clone)]
pub struct FsTree {
    nodes: Vec<Node>,
}

impl Default for FsTree {
    fn default() -> Self {
        Self::new()
    }
}

impl FsTree {
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                name: String::new(),
                is_dir: true,
                data: Vec::new(),
                deleted: false,
                fragmented: false,
                parent: None,
                children: Vec::new(),
            }],
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, id: usize) -> &Node {
        &self.nodes[id]
    }

    /// パスのディレクトリを作る(途中のディレクトリも作る)。
    pub fn dir(&mut self, path: &str) -> usize {
        let mut current = 0;
        for part in split(path) {
            current = self.child_or_create(current, part, true);
        }
        current
    }

    /// ファイルを置く。
    pub fn file(&mut self, path: &str, data: Vec<u8>) -> usize {
        let (parent_path, name) = split_last(path);
        let parent = if parent_path.is_empty() {
            0
        } else {
            self.dir(&parent_path)
        };
        let id = self.child_or_create(parent, &name, false);
        self.nodes[id].data = data;
        id
    }

    /// 削除済みにする。
    pub fn delete(&mut self, path: &str) {
        if let Some(id) = self.find(path) {
            self.nodes[id].deleted = true;
        }
    }

    /// 断片化して配置する。
    pub fn fragment(&mut self, path: &str) {
        if let Some(id) = self.find(path) {
            self.nodes[id].fragmented = true;
        }
    }

    /// パスから ID を引く。
    pub fn find(&self, path: &str) -> Option<usize> {
        let mut current = 0;
        for part in split(path) {
            current = *self.nodes[current]
                .children
                .iter()
                .find(|&&c| self.nodes[c].name == part)?;
        }
        Some(current)
    }

    /// ルートからのパス。
    pub fn path_of(&self, id: usize) -> String {
        let mut parts = Vec::new();
        let mut current = Some(id);
        while let Some(i) = current {
            if i == 0 {
                break;
            }
            parts.push(self.nodes[i].name.as_str());
            current = self.nodes[i].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn child_or_create(&mut self, parent: usize, name: &str, is_dir: bool) -> usize {
        if let Some(&id) = self.nodes[parent]
            .children
            .iter()
            .find(|&&c| self.nodes[c].name == name)
        {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_string(),
            is_dir,
            data: Vec::new(),
            deleted: false,
            fragmented: false,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.nodes[parent].children.push(id);
        id
    }
}

fn split(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|p| !p.is_empty())
}

fn split_last(path: &str) -> (String, String) {
    let parts: Vec<&str> = split(path).collect();
    match parts.split_last() {
        Some((name, rest)) => (rest.join("/"), (*name).to_string()),
        None => (String::new(), String::new()),
    }
}
