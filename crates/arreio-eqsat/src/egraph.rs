//! E-Graph — grafo de equivalência para Equality Saturation.

use crate::language::Expr;

use std::collections::HashMap;

/// Identificador de uma e-class.
pub type EClassId = usize;

/// Nodo no e-graph (operador + filhos canonicalizados).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ENode {
    pub op: String,
    pub children: Vec<EClassId>,
}

impl ENode {
    pub fn new(op: impl Into<String>, children: Vec<EClassId>) -> Self {
        Self {
            op: op.into(),
            children,
        }
    }
}

/// E-class: conjunto de nodos equivalentes.
#[derive(Debug, Clone)]
pub struct EClass {
    pub id: EClassId,
    pub nodes: Vec<ENode>,
    pub parents: Vec<(EClassId, ENode)>,
}

/// Union-find simples com path compression.
#[derive(Debug, Clone, Default)]
pub struct UnionFind {
    pub(crate) parent: Vec<EClassId>,
}

impl UnionFind {
    pub fn new() -> Self {
        Self { parent: Vec::new() }
    }

    pub fn make_set(&mut self) -> EClassId {
        let id = self.parent.len();
        self.parent.push(id);
        id
    }

    pub fn find(&mut self, id: EClassId) -> EClassId {
        if self.parent[id] == id {
            return id;
        }
        let root = self.find(self.parent[id]);
        self.parent[id] = root;
        root
    }

    pub fn union(&mut self, a: EClassId, b: EClassId) -> EClassId {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return ra;
        }
        self.parent[rb] = ra;
        ra
    }
}

/// E-Graph: conjunto de e-classes com union-find.
#[derive(Debug, Clone)]
pub struct EGraph {
    pub classes: HashMap<EClassId, EClass>,
    pub union_find: UnionFind,
    /// Mapeamento de nodo canonicalizado para e-class para garantir hashconsing.
    hashcons: HashMap<ENode, EClassId>,
    /// Próximo id disponível.
    next_id: EClassId,
}

impl EGraph {
    pub fn new() -> Self {
        Self {
            classes: HashMap::new(),
            union_find: UnionFind::new(),
            hashcons: HashMap::new(),
            next_id: 0,
        }
    }

    fn fresh_id(&mut self) -> EClassId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Canonicaliza um id via union-find.
    pub fn find(&mut self, id: EClassId) -> EClassId {
        self.union_find.find(id)
    }

    /// Adiciona uma expressão ao e-graph, retornando o id da e-class raiz.
    pub fn add_expr(&mut self, expr: &Expr) -> EClassId {
        match expr {
            Expr::Const(c) => {
                let node = ENode::new("Const", vec![]);
                let _id = self.add_node(node);
                // Guardamos o valor da constante em um nodo adicional para distinção semântica
                // Na prática, constantes diferentes podem ser distintas.
                let const_node = ENode::new(format!("Const:{}", c), vec![]);
                self.add_node(const_node)
            }
            Expr::Var(v) => {
                let node = ENode::new(format!("Var:{}", v), vec![]);
                self.add_node(node)
            }
            Expr::Add(a, b) => {
                let cid_a = self.add_expr(a);
                let cid_b = self.add_expr(b);
                let node = ENode::new("Add", vec![cid_a, cid_b]);
                self.add_node(node)
            }
            Expr::Mul(a, b) => {
                let cid_a = self.add_expr(a);
                let cid_b = self.add_expr(b);
                let node = ENode::new("Mul", vec![cid_a, cid_b]);
                self.add_node(node)
            }
            Expr::Neg(a) => {
                let cid = self.add_expr(a);
                let node = ENode::new("Neg", vec![cid]);
                self.add_node(node)
            }
        }
    }

    pub(crate) fn add_node(&mut self, node: ENode) -> EClassId {
        if let Some(&id) = self.hashcons.get(&node) {
            return id;
        }
        let id = self.fresh_id();
        self.union_find.make_set();
        let eclass = EClass {
            id,
            nodes: vec![node.clone()],
            parents: vec![],
        };
        self.classes.insert(id, eclass);
        self.hashcons.insert(node, id);
        id
    }

    /// Une duas e-classes, retornando o id representante.
    pub fn merge(&mut self, id1: EClassId, id2: EClassId) -> EClassId {
        let r1 = self.find(id1);
        let r2 = self.find(id2);
        if r1 == r2 {
            return r1;
        }
        let new_root = self.union_find.union(r1, r2);
        let other = if new_root == r1 { r2 } else { r1 };

        // Move nodos e pais da e-class absorvida para a nova raiz.
        let mut other_class = self.classes.remove(&other).unwrap_or_else(|| EClass {
            id: other,
            nodes: vec![],
            parents: vec![],
        });

        if let Some(root_class) = self.classes.get_mut(&new_root) {
            root_class.nodes.append(&mut other_class.nodes);
            root_class.parents.append(&mut other_class.parents);
        } else {
            // Caso raro: root não existe mais (não deve acontecer com union-find correto)
            self.classes.insert(
                new_root,
                EClass {
                    id: new_root,
                    nodes: other_class.nodes,
                    parents: other_class.parents,
                },
            );
        }

        new_root
    }

    /// Restaura invariantes do e-graph após merges.
    /// Canonicaliza filhos dos nodos e consolida nodos idênticos.
    pub fn rebuild(&mut self) {
        // Coleta todos os dados necessários antes de modificar.
        let all_nodes: Vec<(EClassId, Vec<ENode>)> = self
            .classes
            .iter()
            .map(|(&id, class)| (id, class.nodes.clone()))
            .collect();

        // Coleta todos os pares (class_id, enode_antigo, enode_novo) que precisam ser atualizados.
        let mut updates: Vec<(EClassId, ENode, ENode)> = Vec::new();
        for (class_id, nodes) in &all_nodes {
            for node in nodes {
                let mut new_node = node.clone();
                let mut changed = false;
                for child in &mut new_node.children {
                    let canonical = self.find(*child);
                    if canonical != *child {
                        *child = canonical;
                        changed = true;
                    }
                }
                if changed {
                    updates.push((*class_id, node.clone(), new_node));
                }
            }
        }

        // Atualiza hashcons: remove nodos antigos e insere novos.
        for (class_id, old_node, new_node) in updates {
            self.hashcons.remove(&old_node);
            if let Some(&existing) = self.hashcons.get(&new_node) {
                // Nodo idêntico já existe em outra e-class: unifica.
                let existing_root = self.find(existing);
                let class_root = self.find(class_id);
                if existing_root != class_root {
                    self.merge(existing_root, class_root);
                }
            } else {
                let canonical_id = self.find(class_id);
                self.hashcons.insert(new_node.clone(), canonical_id);
                if let Some(class) = self.classes.get_mut(&canonical_id) {
                    if let Some(pos) = class.nodes.iter().position(|n| *n == old_node) {
                        class.nodes[pos] = new_node;
                    }
                }
            }
        }

        // Reconstroi pais corretamente.
        for class in self.classes.values_mut() {
            class.parents.clear();
        }
        let all_entries: Vec<(EClassId, Vec<ENode>)> = self
            .classes
            .iter()
            .map(|(&id, class)| (id, class.nodes.clone()))
            .collect();
        for (class_id, nodes) in all_entries {
            let canonical_id = self.find(class_id);
            for node in nodes {
                for child in &node.children {
                    let child_canonical = self.find(*child);
                    if let Some(child_class) = self.classes.get_mut(&child_canonical) {
                        child_class.parents.push((canonical_id, node.clone()));
                    }
                }
            }
        }
    }

    /// Retorna todos os nodos de uma e-class (canonicalizada).
    pub fn get_class(&self, id: EClassId) -> Option<&EClass> {
        let root = self.union_find.parent.get(id).copied().unwrap_or(id);
        self.classes.get(&root)
    }
}

impl Default for EGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egraph_add_const() {
        let mut g = EGraph::new();
        let expr = Expr::Const(7);
        let id = g.add_expr(&expr);
        assert_eq!(g.find(id), id);
        let class = g.get_class(id).unwrap();
        assert!(!class.nodes.is_empty());
    }

    #[test]
    fn egraph_add_expr() {
        let mut g = EGraph::new();
        let expr = Expr::Add(Box::new(Expr::Const(1)), Box::new(Expr::Const(2)));
        let id = g.add_expr(&expr);
        let class = g.get_class(id).unwrap();
        assert_eq!(class.nodes.len(), 1);
        assert_eq!(class.nodes[0].op, "Add");
        assert_eq!(class.nodes[0].children.len(), 2);
    }

    #[test]
    fn egraph_merge_unifies() {
        let mut g = EGraph::new();
        let id1 = g.add_expr(&Expr::Const(1));
        let _id2 = g.add_expr(&Expr::Const(1));
        // Duas constantes iguais são hashconsed automaticamente, então testamos com merge forçado
        let id3 = g.add_expr(&Expr::Const(2));
        let root = g.merge(id1, id3);
        assert_eq!(g.find(id1), root);
        assert_eq!(g.find(id3), root);
    }

    #[test]
    fn egraph_find_after_merge() {
        let mut g = EGraph::new();
        let id1 = g.add_expr(&Expr::Var("x".to_string()));
        let id2 = g.add_expr(&Expr::Var("y".to_string()));
        let root = g.merge(id1, id2);
        g.rebuild();
        assert_eq!(g.find(id1), root);
        assert_eq!(g.find(id2), root);
    }

    #[test]
    fn egraph_rebuild_hashcons() {
        let mut g = EGraph::new();
        let a = g.add_expr(&Expr::Const(1));
        let b = g.add_expr(&Expr::Const(2));
        let add1 = g.add_expr(&Expr::Add(
            Box::new(Expr::Const(1)),
            Box::new(Expr::Const(2)),
        ));

        // Força um merge que pode criar duplicação
        g.merge(a, b);
        g.rebuild();

        // Após rebuild, o e-graph deve estar consistente
        let root = g.find(add1);
        let class = g.get_class(root).unwrap();
        assert!(!class.nodes.is_empty());
    }

    #[test]
    fn egraph_parents_updated() {
        let mut g = EGraph::new();
        let _a = g.add_expr(&Expr::Var("a".to_string()));
        let _b = g.add_expr(&Expr::Var("b".to_string()));
        let add = g.add_expr(&Expr::Add(
            Box::new(Expr::Var("a".to_string())),
            Box::new(Expr::Var("b".to_string())),
        ));
        g.rebuild();

        let add_root = g.find(add);
        let add_class = g.get_class(add_root).unwrap();
        assert_eq!(add_class.nodes[0].op, "Add");
    }
}
