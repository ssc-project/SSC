//! SSC CSS Parser
//!
//! # Performance
//!
//! The following optimization techniques are used:
//! * AST is allocated in a memory arena ([bumpalo](https://docs.rs/bumpalo))
//!   for fast AST drop
//! * Short strings are inlined by [CompactString](https://github.com/ParkMyCar/compact_str)
//! * No other heap allocations are done except the above two
//! * [oxc_span::Span] offsets uses `u32` instead of `usize`
//!
//! # Usage
//!
//! The parser has a minimal API with two inputs and one return struct
//! ([ParserReturn]).
//!
//! ```rust
//! let parser_return = Parser::new(&allocator, &source_text).parse();
//! ```
//!
//! # Visitor
//!
//! See [ssc_css_ast::Visit] and [ssc_css_ast::VisitMut]

#![allow(clippy::wildcard_imports)] // allow for use `ssc_css_ast::ast::*`

mod cursor;

mod block;
mod rule;
mod selector;
mod value;

mod diagnostics;

// Expose lexer only in benchmarks
#[cfg(not(feature = "benchmarking"))]
mod lexer;
#[cfg(feature = "benchmarking")]
#[doc(hidden)]
pub mod lexer;

use oxc_allocator::Allocator;
use oxc_diagnostics::{OxcDiagnostic, Result};
use oxc_span::{Atom, Span};
use ssc_css_ast::{ast::StyleSheet, AstBuilder, Trivias};

pub use crate::lexer::Kind; // re-export for codegen
use crate::lexer::{Lexer, Token};

/// Maximum length of source which can be parsed (in bytes).
/// ~4 GiB on 64-bit systems, ~2 GiB on 32-bit systems.
// Length is constrained by 2 factors:
// 1. `Span`'s `start` and `end` are `u32`s, which limits length to `u32::MAX`
//    bytes.
// 2. Rust's allocator APIs limit allocations to `isize::MAX`.
// https://doc.rust-lang.org/std/alloc/struct.Layout.html#method.from_size_align
pub const MAX_LEN: usize = if std::mem::size_of::<usize>() >= 8 {
    // 64-bit systems
    u32::MAX as usize
} else {
    // 32-bit or 16-bit systems
    isize::MAX as usize
};

/// Return value of parser consisting of AST, errors and comments
///
/// The parser always return a valid AST.
/// When `panicked = true`, then stylehsheet will always be empty.
/// When `errors.len() > 0`, then stylesheet may or may not be empty due to error
/// recovery.
pub struct ParserReturn<'a> {
    pub stylesheet: StyleSheet<'a>,
    pub errors: Vec<OxcDiagnostic>,
    pub trivias: Trivias,
    pub panicked: bool,
}

/// Recursive Descent Parser
///
/// See [`Parser::parse`] for entry function.
pub struct Parser<'a> {
    allocator: &'a Allocator,
    source_text: &'a str,
}

impl<'a> Parser<'a> {
    /// Create a new parser
    pub fn new(allocator: &'a Allocator, source_text: &'a str) -> Self {
        Self { allocator, source_text }
    }
}

mod parser_parse {
    use super::*;

    /// `UniquePromise` is a way to use the type system to enforce the invariant
    /// that only a single `ParserImpl`, `Lexer` and `lexer::Source` can
    /// exist at any time on a thread. This constraint is required to
    /// guarantee the soundness of some methods of these types
    /// e.g. `Source::set_position`.
    ///
    /// `ParserImpl::new`, `Lexer::new` and `lexer::Source::new` all require a
    /// `UniquePromise` to be provided to them. `UniquePromise::new` is not
    /// visible outside this module, so only `Parser::parse` can create one,
    /// and it only calls `ParserImpl::new` once. This enforces the
    /// invariant throughout the entire parser.
    ///
    /// `UniquePromise` is a zero-sized type and has no runtime cost. It's
    /// purely for the type-checker.
    ///
    /// `UniquePromise::new_for_tests` is a backdoor for unit tests and
    /// benchmarks, so they can create a `ParserImpl` or `Lexer`, and
    /// manipulate it directly, for testing/benchmarking purposes.
    pub(crate) struct UniquePromise {
        _dummy: (),
    }

    impl UniquePromise {
        #[inline]
        fn new() -> Self {
            Self { _dummy: () }
        }

        /// Backdoor for tests/benchmarks to create a `UniquePromise` (see
        /// above). This function must NOT be exposed outside of tests
        /// and benchmarks, as it allows circumventing safety invariants
        /// of the parser.
        #[cfg(any(test, feature = "benchmarking"))]
        pub fn new_for_tests() -> Self {
            Self { _dummy: () }
        }
    }

    impl<'a> Parser<'a> {
        /// Main entry point
        ///
        /// Returns an empty `StyleSheet` on unrecoverable error,
        /// Recoverable errors are stored inside `errors`.
        pub fn parse(self) -> ParserReturn<'a> {
            let unique = UniquePromise::new();
            let parser = ParserImpl::new(self.allocator, self.source_text, unique);
            parser.parse()
        }
    }
}
use parser_parse::UniquePromise;

/// Implementation of parser.
/// `Parser` is just a public wrapper, the guts of the implementation is in this
/// type.
struct ParserImpl<'a> {
    lexer: Lexer<'a>,

    /// Source Code
    source_text: &'a str,

    /// All syntax errors from parser and lexer
    /// Note: favor adding to `Diagnostics` instead of raising Err
    errors: Vec<OxcDiagnostic>,

    /// The current parsing token
    token: Token,

    /// The end range of the previous token
    prev_token_end: u32,

    /// Ast builder for creating AST spans
    ast: AstBuilder<'a>,
}

impl<'a> ParserImpl<'a> {
    /// Create a new `ParserImpl`.
    ///
    /// Requiring a `UniquePromise` to be provided guarantees only 1
    /// `ParserImpl` can exist on a single thread at one time.
    #[inline]
    pub fn new(allocator: &'a Allocator, source_text: &'a str, unique: UniquePromise) -> Self {
        Self {
            lexer: Lexer::new(allocator, source_text, unique),
            source_text,
            errors: vec![],
            token: Token::default(),
            prev_token_end: 0,
            ast: AstBuilder::new(allocator),
        }
    }

    /// Backdoor to create a `ParserImpl` without holding a `UniquePromise`, for
    /// unit tests. This function must NOT be exposed in public API as it
    /// breaks safety invariants.
    #[cfg(test)]
    #[allow(unused)]
    fn new_for_tests(allocator: &'a Allocator, source_text: &'a str) -> Self {
        let unique = UniquePromise::new_for_tests();
        Self::new(allocator, source_text, unique)
    }

    /// Main entry point
    ///
    /// Returns an empty `Program` on unrecoverable error,
    /// Recoverable errors are stored inside `errors`.
    #[inline]
    pub fn parse(mut self) -> ParserReturn<'a> {
        let (program, panicked) = match self.parse_stylesheet() {
            Ok(stylesheet) => (stylesheet, false),
            Err(error) => {
                self.error(self.overlong_error().unwrap_or(error));
                let stylesheet =
                    self.ast.stylesheet(Span::default(), self.ast.new_vec(), Atom::from(""));
                (stylesheet, true)
            }
        };
        let errors = self.lexer.errors.into_iter().chain(self.errors).collect();
        let trivias = self.lexer.trivia_builder.build();
        ParserReturn { stylesheet: program, errors, trivias, panicked }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn parse_stylesheet(&mut self) -> Result<StyleSheet<'a>> {
        // initialize cur_token and prev_token by moving onto the first token
        let span = self.start_span();
        self.bump_any();

        let children = self.parse_rules()?;

        let span = self.end_span(span);

        Ok(self.ast.stylesheet(
            span,
            children,
            Atom::from(&self.source_text[(span.start as usize)..(span.end as usize)]),
        ))
    }

    /// Check if source length exceeds MAX_LEN, if the file cannot be parsed.
    /// Original parsing error is not real - `Lexer::new` substituted "\0" as
    /// the source text.
    fn overlong_error(&self) -> Option<OxcDiagnostic> {
        if self.source_text.len() > MAX_LEN {
            return Some(diagnostics::overlong_source());
        }
        None
    }

    /// Return error info at current token
    /// # Panics
    ///   * The lexer did not push a diagnostic when `Kind::Undetermined` is
    ///     returned
    fn unexpected(&mut self) -> OxcDiagnostic {
        // The lexer should have reported a more meaningful diagnostic
        // when it is a undetermined kind.
        if self.cur_kind() == Kind::Undetermined {
            if let Some(error) = self.lexer.errors.pop() {
                return error;
            }
        }
        diagnostics::unexpected_token(self.cur_token().span())
    }

    /// Push a Syntax Error
    fn error(&mut self, error: OxcDiagnostic) {
        self.errors.push(error);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn smoke_test() {
        let allocator = Allocator::default();
        let source = "";
        let ret = Parser::new(&allocator, source).parse();
        assert!(ret.stylesheet.children.is_empty());
        assert!(ret.errors.is_empty());
    }

    #[test]
    fn comments() {
        let allocator = Allocator::default();
        let sources = [
            "/* line comment */",
            "p {
                color: /* informational comment */ blue;
            }",
        ];
        for source in sources {
            let ret = Parser::new(&allocator, source).parse();
            let comments = ret.trivias.comments().collect::<Vec<_>>();
            assert_eq!(comments.len(), 1, "{source}");
        }
    }

    // Source with length MAX_LEN + 1 fails to parse.
    // Skip this test on 32-bit systems as impossible to allocate a string
    // longer than `isize::MAX`.
    #[cfg(not(debug_assertions))]
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn overlong_source() {
        // Build string in 16 KiB chunks for speed
        let mut source = String::with_capacity(MAX_LEN + 1);
        let line = "p { color: red }\n";
        let chunk = line.repeat(1024);
        while source.len() < MAX_LEN + 1 - chunk.len() {
            source.push_str(&chunk);
        }
        while source.len() < MAX_LEN + 1 - line.len() {
            source.push_str(line);
        }
        while source.len() < MAX_LEN + 1 {
            source.push('\n');
        }
        assert_eq!(source.len(), MAX_LEN + 1);

        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, &source).parse();
        assert!(ret.stylesheet.children.is_empty());
        assert!(ret.panicked);
        assert_eq!(ret.errors.len(), 1);
        assert_eq!(ret.errors.first().unwrap().to_string(), "Source length exceeds 4 GiB limit");
    }

    // Source with length MAX_LEN parses OK.
    // This test takes over 1 minute on an M1 Macbook Pro unless compiled in
    // release mode. `not(debug_assertions)` is a proxy for detecting
    // release mode.
    #[cfg(not(debug_assertions))]
    #[test]
    fn legal_length_source() {
        // Build a string MAX_LEN bytes long which doesn't take too long to
        // parse
        let head = "p { color: red }\n/*";
        let foot = "*/\nbutton { color: blue }\n";
        let mut source = "x".repeat(MAX_LEN);
        source.replace_range(..head.len(), head);
        source.replace_range(MAX_LEN - foot.len().., foot);
        assert_eq!(source.len(), MAX_LEN);

        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, &source).parse();
        assert!(!ret.panicked);
        assert!(ret.errors.is_empty());
        assert_eq!(ret.stylesheet.children.len(), 2);
    }
}
