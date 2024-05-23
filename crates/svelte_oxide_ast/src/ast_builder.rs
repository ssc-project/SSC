use std::mem;

use oxc_allocator::{Allocator, Box, String, Vec};
use oxc_span::{Atom, Span};
use svelte_oxide_css_ast::ast::StyleSheet;

use crate::ast::*;

pub struct AstBuilder<'a> {
    pub allocator: &'a Allocator,
}

impl<'a> AstBuilder<'a> {
    pub fn new(allocator: &'a Allocator) -> Self {
        Self { allocator }
    }

    #[inline]
    pub fn alloc<T>(&self, value: T) -> Box<'a, T> {
        Box::new_in(value, self.allocator)
    }

    #[inline]
    pub fn new_vec<T>(&self) -> Vec<'a, T> {
        Vec::new_in(self.allocator)
    }

    #[inline]
    pub fn new_vec_with_capacity<T>(&self, capacity: usize) -> Vec<'a, T> {
        Vec::with_capacity_in(capacity, self.allocator)
    }

    #[inline]
    pub fn new_vec_single<T>(&self, value: T) -> Vec<'a, T> {
        let mut vec = self.new_vec_with_capacity(1);
        vec.push(value);
        vec
    }

    #[inline]
    pub fn new_vec_from_iter<T, I: IntoIterator<Item = T>>(
        &self,
        iter: I,
    ) -> Vec<'a, T> {
        Vec::from_iter_in(iter, self.allocator)
    }

    #[inline]
    pub fn new_str(&self, value: &str) -> &'a str {
        String::from_str_in(value, self.allocator).into_bump_str()
    }

    #[inline]
    pub fn new_atom(&self, value: &str) -> Atom<'a> {
        Atom::from(String::from_str_in(value, self.allocator).into_bump_str())
    }

    pub fn copy<T>(&self, src: &T) -> T {
        // SAFETY:
        // This should be safe as long as `src` is an reference from the
        // allocator. But honestly, I'm not really sure if this is safe.
        #[allow(unsafe_code)]
        unsafe {
            mem::transmute_copy(src)
        }
    }

    pub fn root(
        &self,
        span: Span,
        fragment: Fragment<'a>,
        css: Option<StyleSheet<'a>>,
        instance: Option<Script<'a>>,
        module: Option<Script<'a>>,
        ts: bool,
    ) -> Root<'a> {
        Root {
            span,
            options: None,
            fragment,
            css,
            instance,
            module,
            metadata: RootMetadata { ts },
        }
    }

    pub fn fragment(
        &self,
        nodes: Vec<'a, FragmentNodeKind<'a>>,
        transparent: bool,
    ) -> Fragment<'a> {
        Fragment { nodes, transparent }
    }
}
