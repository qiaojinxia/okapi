"""Protect the inventory against silent omissions that affected the old probe."""
import importlib.util
from pathlib import Path
import sys
import unittest

spec = importlib.util.spec_from_file_location('api_surface', Path(__file__).with_name('api-surface.py'))
surface = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = surface
spec.loader.exec_module(surface)


class ApiSurfaceTest(unittest.TestCase):
    def extract(self, source, role='console'):
        return surface.extract(source, role, 'router.rs')

    def test_comments_strings_and_nested_calls_cannot_invent_routes_or_methods(self):
        source = '''
        // .route("/comment", post(nope))
        /* outer /* .route("/nested", delete(nope)) */ end */
        let text = r#".route("/string", put(nope))"#;
        Router::new().route("/actual", get(|| async { format!("post( { )") }))
          .route("/write", post(write).delete(remove));
        do_something.get(key);
        '''
        self.assertEqual([(row['path'], row['method']) for row in self.extract(source)],
                         [('/actual', 'GET'), ('/actual', 'HEAD'), ('/write', 'POST'), ('/write', 'DELETE')])

    def test_any_and_explicit_head_are_not_omitted(self):
        rows = self.extract('Router::new().route("/pass/{id}/{*path}", axum::routing::any(pass))')
        self.assertEqual({row['method'] for row in rows}, set(surface.METHODS))
        self.assertTrue(all(row['registration'] == 'any' for row in rows))
        rows = self.extract('Router::new().route("/x", get(read).head(headers))')
        self.assertEqual([(row['method'], row['handler']) for row in rows], [('GET', 'read'), ('HEAD', 'headers')])

    def test_realtime_stats_and_separate_surfaces_remain_in_inventory(self):
        source = 'Router::new().route("/healthz", get(health)).route("/admin/stats/realtime", get(stats))'
        rows = self.extract(source) + self.extract(source, 'gateway')
        self.assertEqual(len({(r['surface'], r['method'], r['path']) for r in rows}), 8)

    def test_multiline_raw_paths_and_non_ascii_comments_keep_source_location(self):
        rows = self.extract('''// 中文说明\nRouter::new().route(\n r#"/v1/models"#,\n get(models),\n)''')
        self.assertEqual(rows[0]['path'], '/v1/models')
        self.assertEqual(rows[0]['line'], 2)

    def test_unknown_syntax_fails_instead_of_reporting_partial_coverage(self):
        for source in ['r.route(path, get(handler))', 'r.route("/x", unknown(handler))',
                       'r.route("/x", router_variable)', 'r.route("/x", get(handler)',
                       'r.nest("/prefix", subrouter)', 'r.route_service("/x", service)',
                       'r.route::<State>("/x", get(handler))']:
            with self.subTest(source=source), self.assertRaises((ValueError, IndexError)):
                self.extract(source)


if __name__ == '__main__':
    unittest.main()
