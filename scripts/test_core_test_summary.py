"""Do not let truncated logs or soft skips become a full-pass claim."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('core_summary', Path(__file__).with_name('summarize-core-tests.py'))
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)

GREEN = '''    Running tests/example.rs (target/debug/deps/example-123)
running 1 test
test example ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
'''


class CoreSummaryTest(unittest.TestCase):
    def test_exit_must_be_observed_even_when_log_is_green(self):
        self.assertEqual(summary.summarize(GREEN)['execution_status'], 'incomplete')
        self.assertEqual(summary.summarize(GREEN, 0)['execution_status'], 'passed')
        self.assertEqual(summary.summarize(GREEN, 101)['execution_status'], 'failed')

    def test_unfinished_suite_or_compile_only_log_cannot_pass(self):
        for log in [GREEN + 'Running tests/next.rs (target/debug/deps/next-1)\n', 'Compiling app\n']:
            self.assertEqual(summary.summarize(log, 0)['execution_status'], 'incomplete')

    def test_probe_on_test_name_line_is_preserved(self):
        probe = '{"surface":"console","method":"GET","path":"/admin/users","phase":"anonymous","status":401,"evidence":"authentication_denied"}'
        report = summary.summarize(GREEN.replace('test example ... ok', f'test example ... API_PROBE {probe}\nok'), 0)
        self.assertEqual(report['api_probe_counts'], {'anonymous:authentication_denied': 1})
        self.assertEqual(report['suites'][0]['tests'], [{'name': 'example', 'outcome': 'ok'}])
        self.assertIn('not proof', report['scope'])

    def test_malformed_probe_is_not_silently_omitted(self):
        report = summary.summarize(GREEN.replace('test example ... ok', 'test example ... API_PROBE {}\nok'), 0)
        self.assertEqual(report['execution_status'], 'incomplete')
        self.assertEqual(len(report['parse_errors']), 1)

    def test_soft_skip_and_ignored_tests_remain_visible(self):
        for text in ['跳过：未配置 OKAPI_NATS_URL', 'skipping: no service']:
            report = summary.summarize(GREEN.replace('test example ... ok', f'test example ... {text}\nok'), 0)
            self.assertEqual(report['execution_status'], 'passed_with_skips_or_skip_signals')
            self.assertEqual(len(report['skip_signals']), 1)
        report = summary.summarize(GREEN.replace('1 passed; 0 failed; 0 ignored', '0 passed; 0 failed; 1 ignored').replace('... ok', '... ignored'), 0)
        self.assertEqual(report['execution_status'], 'passed_with_skips_or_skip_signals')

    def test_failed_and_doc_suites_are_separate(self):
        failed = GREEN.replace('example', 'failing').replace('... ok', '... FAILED').replace('ok. 1 passed; 0 failed', 'FAILED. 0 passed; 1 failed')
        doc = GREEN.replace('Running tests/example.rs (target/debug/deps/example-123)', 'Doc-tests okapi')
        report = summary.summarize('\x1b[32m' + GREEN + '\x1b[0m' + failed + doc, 101)
        self.assertEqual(report['totals']['passed'], 2)
        self.assertEqual(report['totals']['failed'], 1)
        self.assertEqual(len(report['suites']), 3)
        self.assertEqual(report['execution_status'], 'failed')

    def test_interleaved_cargo_header_does_not_hide_a_completed_suite(self):
        # stdout's final result is interrupted by Cargo's stderr header.
        next_suite = GREEN.replace('example', 'next')
        interrupted = GREEN.replace(
            '; finished in 0.01s\n',
            '     Running tests/next.rs (target/debug/deps/next-123)\n; finished in 0.01s\n',
        )
        log = interrupted + next_suite.split('\n', 1)[1]
        report = summary.summarize(log, 0)
        self.assertEqual(report['execution_status'], 'passed')
        self.assertEqual(report['totals']['passed'], 2)
        self.assertEqual(len(report['suites']), 2)
        self.assertEqual(report['suites'][1]['tests'][0]['name'], 'next')

    def test_duplicate_results_are_not_silently_overwritten(self):
        report = summary.summarize(GREEN + GREEN.splitlines()[-1] + '\n', 0)
        self.assertEqual(report['execution_status'], 'incomplete')
        self.assertTrue(report['parse_errors'])

    def test_missing_test_outcomes_or_inconsistent_counts_cannot_pass(self):
        for log in [
            GREEN.replace('test example ... ok\n', ''),
            GREEN.replace('test example ... ok', 'test example ...'),
            GREEN.replace('1 passed', '2 passed'),
        ]:
            with self.subTest(log=log):
                report = summary.summarize(log, 0)
                self.assertEqual(report['execution_status'], 'incomplete')
                self.assertTrue(report['parse_errors'])


if __name__ == '__main__':
    unittest.main()
