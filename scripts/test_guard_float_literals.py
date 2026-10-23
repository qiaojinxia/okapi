"""The billing float guard must see bare literals, and must not trip over text that merely looks numeric."""
import importlib.util
from pathlib import Path
import sys
import unittest

spec = importlib.util.spec_from_file_location('guard_float_literals', Path(__file__).with_name('guard-float-literals.py'))
guard = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = guard
spec.loader.exec_module(guard)


def literals(source):
    return [literal for _, literal in guard.float_literals(source)]


class FloatLiteralGuardTest(unittest.TestCase):
    def test_bare_literals_are_found_in_every_spelling(self):
        source = '''
        let a = 0.5;
        let b = 1e3;
        let c = 2.0_f32;
        let d = 1f64;
        let e = 1.;
        let f = 3.14159;
        let g = 6.02e-23;
        '''
        self.assertEqual(literals(source), ['0.5', '1e3', '2.0_f32', '1f64', '1.', '3.14159', '6.02e-23'])

    def test_line_numbers_survive_stripped_comments_and_strings(self):
        source = 'fn f() {\n    /* a\n       b */\n    let s = "x\\\ny";\n    let bad = 0.25;\n}\n'
        self.assertEqual(guard.float_literals(source), [(6, '0.25')])

    def test_comments_strings_and_chars_are_not_code(self):
        source = '''
        // 0.5 in a comment, and 1e3
        /// doc: ratio 1.25
        /* block 2.5 /* nested 3.5 */ still 4.5 */
        let plain = "1.25 and 0.5 and 1e3";
        let raw = r#"4.5 "quoted" 6.5"#;
        let bytes = b"7.5";
        let dot = '.';
        let escaped = '\\'';
        '''
        self.assertEqual(literals(source), [])

    def test_integers_ranges_tuple_fields_and_hex_are_not_floats(self):
        source = '''
        let a = pair.0.1;
        let b = 0..5;
        let c = 1.max(2);
        let d = 0x1e5;
        let e = 1_000_000;
        let f = e6_to_dec(1);
        let g = x.0;
        let lifetime: &'a str = "";
        '''
        self.assertEqual(literals(source), [])

    def test_directory_scan_reports_file_line_and_literal(self):
        import contextlib
        import io
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, 'clean.rs').write_text('fn f() -> i64 { 1_000_000 }\n')
            Path(tmp, 'dirty.rs').write_text('fn g() -> i64 {\n    let x = 0.5;\n    1\n}\n')
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = guard.main([tmp, tmp + '/missing'])
        self.assertEqual(code, 1)
        self.assertIn('dirty.rs:2: 浮点字面量 0.5', out.getvalue())
        self.assertNotIn('clean.rs', out.getvalue())


if __name__ == '__main__':
    unittest.main()
