"""Session-wide request accounting through the real CLI, using synthetic servers only.
Run with GROK_TEST_BINARY inside a network-disabled container. No real session data.
"""
import json
import os
from pathlib import Path
import time
import unittest
import test_provider_cli as protocol


class AccountingHandler(protocol.Handler):
    def do_POST(self):
        if not self.path.endswith('/responses'):
            return super().do_POST()
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or '{}')
        owner = self.server.owner
        owner.aux_requests.append(body)
        response = {'id': 'synthetic-summary', 'object': 'response', 'created_at': 0,
                    'model': body.get('model', 'synthetic-aux'), 'status': 'completed',
                    'output': [{'type': 'function_call', 'id': 'title-call', 'call_id': 'title-call',
                                'name': 'session_title', 'arguments': '{"session_title":"Synthetic title"}',
                                'status': 'completed'}],
                    'usage': {'input_tokens': 20, 'output_tokens': 4, 'total_tokens': 24,
                              'input_tokens_details': {'cached_tokens': 5, 'cache_write_tokens': 0},
                              'output_tokens_details': {'reasoning_tokens': 2},
                              'cost_in_usd_ticks': 50_000_000}}
        payload = ('data: '+json.dumps({'type': 'response.completed', 'sequence_number': 1,
                                      'response': response})+'\n\n') if body.get('stream') else json.dumps(response)
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream' if body.get('stream') else 'application/json')
        self.end_headers()
        try:
            self.wfile.write(payload.encode())
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


class RequestUsageCli(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = Path(os.environ['GROK_TEST_BINARY']).resolve(strict=True)

    def run_case(self, **kwargs):
        case = protocol.ProviderCli('runTest')
        case.binary = self.binary
        case.aux_requests = []
        captured = {}
        def inspect(root, ledger, result, owner):
            captured['aux_requests'] = list(owner.aux_requests)
            captured['updates'] = []
            for path in (root/'grok').rglob('updates.jsonl'):
                captured['updates'].extend(json.loads(line) for line in path.read_text().splitlines() if line.strip())
        if kwargs.pop('partial_timeout', False):
            case.partial_timeout_usage = {'prompt_tokens': 7, 'completion_tokens': 1, 'total_tokens': 8,
                                          'prompt_tokens_details': {'cached_tokens': 0, 'cache_write_tokens': 0},
                                          'completion_tokens_details': {'reasoning_tokens': 1}, 'cost': .002}
            def before_stop(root, owner):
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    for path in (root/'grok').rglob('usage.json'):
                        requests = json.loads(path.read_text()).get('request_usage', {}).get('calls', [])
                        if any(row.get('usage', {}).get('input_tokens') == 7 for row in requests if row.get('usage')):
                            return
                    time.sleep(.02)
                self.fail('partial streamed usage was not checkpointed before timeout')
            kwargs.update(timeout=True, timeout_command=True, before_stop=before_stop)
        ledger, result = case.run_case(handler_class=AccountingHandler, inspect=inspect, **kwargs)
        return ledger, result, captured

    def check_main(self, requests, expected_calls=2):
        self.assertEqual(requests['schema_version'], 1)
        self.assertTrue(requests['history_complete'])
        self.assertEqual(requests['recording_errors'], 0)
        calls = requests['calls']
        self.assertEqual(len({row['call_id'] for row in calls}), len(calls))
        main = [row for row in calls if row['purpose'] == 'main_loop']
        self.assertEqual(len(main), expected_calls)
        self.assertTrue(all(row['origin_session_id'] and row['prompt_id'] for row in main))
        encoded = json.dumps(requests)
        for private in ('Synthetic protocol check.', 'synthetic-tool-ok', 'synthetic-provider-key',
                        'synthetic-xai-must-not-leak', '127.0.0.1', 'Synthetic title'):
            self.assertNotIn(private, encoded)
        return main

    def test_main_and_auxiliary_are_separate_on_both_terminal_surfaces(self):
        for output in ('json', 'streaming-messages-json'):
            with self.subTest(output=output):
                ledger, result, captured = self.run_case(output=output)
                requests = ledger['request_usage']
                main = self.check_main(requests)
                self.assertEqual([row['usage']['input_tokens'] for row in main], [100, 50])
                self.assertEqual([row['usage']['reasoning_tokens'] for row in main], [3, 1])
                self.assertEqual(requests['summary']['main']['total_tokens']['total'], 165)
                self.assertEqual(requests['summary']['main']['uncached_input_tokens']['total'], 113)
                self.assertAlmostEqual(requests['summary']['main']['cost']['total_usd'], .03)
                self.assertGreaterEqual(len(captured['aux_requests']), 1)
                aux = [row for row in requests['calls'] if row['purpose'] != 'main_loop']
                self.assertEqual(len(aux), len(captured['aux_requests']))
                self.assertTrue(all(row['purpose'] == 'session_summary' for row in aux))
                self.assertTrue(all(row['provider'] == 'xai' for row in aux))
                self.assertTrue(all(row['status'] == 'completed' for row in aux))
                self.assertAlmostEqual(requests['summary']['auxiliary']['cost']['total_usd'], .005*len(aux))
                self.assertAlmostEqual(requests['summary']['all']['cost']['total_usd'], .03+.005*len(aux))
                self.assertAlmostEqual(result['total_cost_usd'], .03, msg='legacy main total must exclude auxiliary cost')
                self.assertEqual(ledger['totals']['model_calls'], 2)
                wire = result['session_requests']
                self.assertEqual(wire, requests)
                def find_request_ledgers(node):
                    if isinstance(node, dict):
                        if 'sessionRequests' in node:
                            yield node['sessionRequests']
                        for value in node.values():
                            yield from find_request_ledgers(value)
                    elif isinstance(node, list):
                        for value in node:
                            yield from find_request_ledgers(value)
                wires = list(find_request_ledgers(captured['updates']))
                self.assertTrue(wires, 'turn-completed update lost session request details')
                self.assertEqual(wires[-1], wire)

    def test_missing_money_and_explicit_zero_remain_distinct(self):
        for options, amount in [({'money':0.0}, 0.0), ({'profile':'vllm'}, None), ({'missing_second':True}, None)]:
            with self.subTest(options=options):
                ledger, result, _ = self.run_case(**options)
                self.check_main(ledger['request_usage'])
                self.assertEqual(ledger['request_usage']['summary']['main']['cost']['total_usd'], amount)
                self.assertEqual(result['total_cost_usd'], amount)
                self.assertEqual(ledger['request_usage']['summary']['main']['total_tokens']['total'], 165)

    def test_real_timeout_keeps_partial_second_request_and_unknown_total(self):
        ledger, result, _ = self.run_case(partial_timeout=True)
        self.assertIsNone(result)
        requests = ledger['request_usage']
        main = self.check_main(requests)
        self.assertEqual(main[0]['status'], 'completed')
        self.assertIn(main[1]['status'], ('pending', 'interrupted'))
        self.assertEqual(main[1]['usage']['input_tokens'], 7)
        self.assertEqual(main[1]['usage']['reasoning_tokens'], 1)
        self.assertAlmostEqual(main[1]['usage']['provider_cost']['usd'], .002)
        summary = requests['summary']['main']
        self.assertTrue(summary['usage_is_incomplete'])
        self.assertIsNone(summary['total_tokens']['total'])
        self.assertEqual(summary['total_tokens']['known_total'], 118)
        self.assertIsNone(summary['cost']['total_usd'])
        self.assertAlmostEqual(summary['cost']['known_usd'], .012)
        self.assertEqual(ledger['totals']['model_calls'], 1)
        self.assertEqual(ledger['totals']['cost_by_source'], {'openrouter_usage_cost':.01})


if __name__ == '__main__':
    unittest.main()
