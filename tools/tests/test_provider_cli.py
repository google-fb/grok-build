"""Full CLI against an in-process synthetic Chat Completions server.

Set GROK_TEST_BINARY to a freshly compiled executable. Run in a network-disabled
container for a transport boundary independent of the CLI's feature flags.
No existing home, credential, session or website data is used.
"""
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import unittest


class Server(http.server.ThreadingHTTPServer):
    allow_reuse_address = True
    daemon_threads = True


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path.endswith('/models'):
            value = {'object': 'list', 'data': []}
        else:
            value = {}
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or '{}')
        if not self.path.endswith('/chat/completions'):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{}')
            return
        owner = self.server.owner
        number = len(owner.requests)
        owner.requests.append((self.path, dict(self.headers.items()), body))
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        def emit(delta=None, finish=None, usage=None):
            chunk = {'id': f'mock-{number}', 'object': 'chat.completion.chunk', 'created': 0,
                     'model': 'synthetic-model', 'choices': [] if delta is None else [
                         {'index': 0, 'delta': delta, 'finish_reason': finish}]}
            if usage is not None:
                chunk['usage'] = usage
            self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode())
            self.wfile.flush()
        try:
            if number == 0:
                available = [t['function'] for t in body.get('tools', []) if t.get('type') == 'function']
                tool = next((t for t in available if 'command' in t.get('parameters', {}).get('properties', {})
                             and t['name'].lower() in ('bash', 'run_terminal_cmd', 'run_terminal_command')), None)
                if tool is None:
                    owner.errors.append('No standard shell function tool in request: '+repr([t['name'] for t in available]))
                    emit({'role': 'assistant', 'content': 'tool unavailable'}, 'stop')
                else:
                    arguments = json.dumps({'command': 'printf synthetic-tool-ok', 'description': 'Synthetic tool check'})
                    emit({'role': 'assistant', 'tool_calls': [{'index': 0, 'id': 'synthetic-call', 'type': 'function',
                          'function': {'name': tool['name'], 'arguments': arguments[:17]}}]})
                    emit({'tool_calls': [{'index': 0, 'function': {'arguments': arguments[17:]}}]})
                    emit({}, 'tool_calls')
            elif owner.timeout:
                partial = getattr(owner, 'partial_timeout_usage', None)
                if partial is not None:
                    emit(usage=partial)
                owner.second_request.set()
                owner.release.wait(40)
                return
            else:
                emit({'role': 'assistant', 'content': 'synthetic final'}, 'stop')
            usage = {'prompt_tokens': 100 if number == 0 else 50,
                     'completion_tokens': 10 if number == 0 else 5,
                     'total_tokens': 110 if number == 0 else 55,
                     'prompt_tokens_details': {'cached_tokens': 20 if number == 0 else 10,
                                              'cache_write_tokens': 5 if number == 0 else 2},
                     'completion_tokens_details': {'reasoning_tokens': 3 if number == 0 else 1}}
            if owner.profile == 'xai':
                usage['cost_in_usd_ticks'] = 100_000_000 if number == 0 else 200_000_000
            elif owner.profile == 'openrouter' and not (owner.missing_second and number > 0):
                usage['cost'] = owner.money if owner.money is not None else (.01 if number == 0 else .02)
            emit(usage=usage)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def isolated_env(root, endpoint):
    env = {'PATH': os.environ['PATH'], 'HOME': str(root/'home'), 'USERPROFILE': str(root/'home'),
           'GROK_HOME': str(root/'grok'), 'TMPDIR': str(root/'tmp'), 'SHELL': '/bin/bash',
           'XAI_API_KEY': 'synthetic-xai-must-not-leak',
           'GROK_CODE_XAI_API_KEY': 'synthetic-legacy-must-not-leak',
           'SYNTHETIC_PROVIDER_KEY': 'synthetic-provider-key',
           'GROK_MAX_RETRIES': '0', 'GROK_TURN_SUMMARY': '0', 'GROK_PROMPT_SUGGESTIONS': 'false',
           'GROK_DISABLE_AUTOUPDATER': '1', 'GROK_TELEMETRY_ENABLED': 'false',
           'GROK_TELEMETRY_TRACE_UPLOAD': 'false', 'GROK_FEEDBACK_ENABLED': 'false',
           'GROK_TRACE_UPLOAD': 'false', 'GROK_INSTRUMENTATION': 'disabled',
           'OTEL_SDK_DISABLED': 'true', 'DISABLE_TELEMETRY': '1', 'DISABLE_FEEDBACK_COMMAND': '1',
           'NO_PROXY': '127.0.0.1,localhost', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_TERMINAL_PROMPT': '0'}
    for key in ('GROK_CLI_CHAT_PROXY_BASE_URL', 'GROK_XAI_API_BASE_URL', 'GROK_MODELS_BASE_URL',
                'GROK_FEEDBACK_BASE_URL', 'GROK_TRACE_UPLOAD_URL', 'GROK_MANAGED_CONFIG_URL',
                'GROK_CODE_WEB_URL', 'GROK_CONVERSATIONS_BASE_URL'):
        env[key] = endpoint
    return env


class ProbeHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.server.requests.append(self.path)
        self.send_response(400)
        self.end_headers()
        self.wfile.write(b'{}')

    do_POST = do_GET


class ProviderCli(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = Path(os.environ['GROK_TEST_BINARY']).resolve(strict=True)

    def test_invalid_provider_never_requests_either_endpoint(self):
        for field, value in [('provider_profile', '"openroute-typo"'),
                             ('supports_reasoning_effort', '"yes"'),
                             ('api_backend', '"typo"'), ('model_provider', '"missing"')]:
            with self.subTest(field=field), tempfile.TemporaryDirectory(prefix='astra-invalid-provider-') as folder:
                root = Path(folder)
                for name in ('home', 'grok', 'work', 'tmp'):
                    (root/name).mkdir()
                servers = [Server(('127.0.0.1', port), ProbeHandler) for port in (28081, 28181)]
                threads = []
                for server in servers:
                    server.requests = []
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    threads.append(thread)
                try:
                    setting = '' if field == 'model_provider' else f'{field} = {value}'
                    provider_id = 'missing' if field == 'model_provider' else 'synthetic'
                    (root/'grok/config.toml').write_text(f'''[model_providers.synthetic]
base_url = "http://127.0.0.1:28081/v1"
api_key = "synthetic-do-not-echo"
{setting}
[model.synthetic]
model_provider = "{provider_id}"
model = "synthetic-model"
''')
                    result = subprocess.run([str(self.binary), '-p', 'Synthetic config check.',
                        '--model', 'synthetic', '--output-format', 'json', '--max-turns', '1',
                        '--disable-web-search'], cwd=root/'work', env=isolated_env(root, 'http://127.0.0.1:28181/v1'),
                        capture_output=True, text=True, timeout=30)
                    output = result.stdout + result.stderr
                    self.assertNotEqual(result.returncode, 0, output)
                    self.assertIn('synthetic', output)
                    self.assertIn(field, output)
                    self.assertNotIn('synthetic-do-not-echo', output)
                    for server in servers:
                        self.assertEqual(server.requests, [], 'invalid config must make no HTTP request')
                finally:
                    for server in servers:
                        server.shutdown()
                        server.server_close()
                    for thread in threads:
                        thread.join(2)

    def test_invalid_connection_keys_never_request_either_endpoint(self):
        cases = []
        for field in ('base_ur', 'api_base', 'base_uri', 'baseURL', 'endpoint_url'):
            cases.append((field, f'''[model_providers.synthetic]
{field} = "http://127.0.0.1:28081/v1"
provider_profile = "openrouter"
env_key = "SYNTHETIC_PROVIDER_KEY"
[model.synthetic]
model_provider = "synthetic"
'''))
        for value in ('["synthetic"]', '7', 'false', '{}', '""', '"  "'):
            cases.append(('model_provider', f'''[model.synthetic]
model_provider = {value}
api_key = "synthetic-do-not-echo"
'''))
        for settings in ('provider_profile = "vllm"', 'provider_profile = "compatible"',
                         'api_key = "synthetic-do-not-echo"', 'base_url = "   "',
                         'provider_profile = "xai"\napi_key = "synthetic-do-not-echo"'):
            cases.append(('base_url', '[model.synthetic]\n'+settings))
        cases.append(("<entry>", "[model]\nsynthetic = false"))
        for field, config in cases:
            with self.subTest(field=field, config=config), tempfile.TemporaryDirectory(prefix='astra-connection-') as folder:
                root = Path(folder)
                for name in ('home', 'grok', 'work', 'tmp'):
                    (root/name).mkdir()
                servers = [Server(('127.0.0.1', port), ProbeHandler) for port in (28081, 28181)]
                threads = []
                for server in servers:
                    server.requests = []
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    threads.append(thread)
                try:
                    (root/'grok/config.toml').write_text(config)
                    result = subprocess.run([str(self.binary), '-p', 'Synthetic connection check.',
                        '--model', 'synthetic', '--output-format', 'json', '--max-turns', '1',
                        '--disable-web-search'], cwd=root/'work', env=isolated_env(root, 'http://127.0.0.1:28181/v1'),
                        capture_output=True, text=True, timeout=30)
                    output = result.stdout + result.stderr
                    self.assertEqual([len(server.requests) for server in servers], [0, 0],
                                     f'{field}: provider/xAI request counts must both be zero')
                    self.assertNotEqual(result.returncode, 0, output)
                    self.assertIn('synthetic', output)
                    self.assertIn(field, output)
                    for secret in ('synthetic-do-not-echo', 'synthetic-provider-key', 'synthetic-xai-must-not-leak'):
                        self.assertNotIn(secret, output)
                    for server in servers:
                        self.assertEqual(server.requests, [], 'invalid connection must make no HTTP request')
                finally:
                    for server in servers:
                        server.shutdown()
                        server.server_close()
                    for thread in threads:
                        thread.join(2)

    def run_case(self, profile='openrouter', output='streaming-messages-json', money=None,
                 timeout=False, missing_second=False, handler_class=Handler, inspect=None,
                 before_stop=None, timeout_command=False):
        self.profile, self.money, self.timeout, self.missing_second = profile, money, timeout, missing_second
        self.requests, self.errors = [], []
        self.second_request, self.release = threading.Event(), threading.Event()
        with tempfile.TemporaryDirectory(prefix='astra-provider-cli-') as folder:
            root = Path(folder)
            for name in ('home', 'grok', 'work', 'tmp'):
                (root/name).mkdir()
            server = Server(('127.0.0.1', 28081), handler_class)
            server.owner = self
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            endpoint = 'http://127.0.0.1:28081/v1'
            credential = '' if profile == 'vllm' else 'env_key = "SYNTHETIC_PROVIDER_KEY"'
            (root/'grok/config.toml').write_text(f'''
[model_providers.synthetic]
base_url = "{endpoint}"
api_backend = "chat_completions"
provider_profile = "{profile}"
{credential}
[model.synthetic]
model_provider = "synthetic"
model = "synthetic-model"
context_window = 32768
[model_providers.unused-typo]
base_ur = "http://127.0.0.1:28181/v1"
[model_providers.unused-invalid]
provider_profile = "bad-profile"
''')
            env = isolated_env(root, endpoint)
            cmd = [str(self.binary), '-p', 'Synthetic protocol check.', '--model', 'synthetic',
                   '--output-format', output, '--max-turns', '3', '--permission-mode', 'bypassPermissions',
                   '--disable-web-search']
            if timeout_command:
                cmd = ['timeout', '--signal=TERM', '--kill-after=3s', '12s', *cmd]
            process = subprocess.Popen(cmd, cwd=root/'work', env=env, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, text=True, start_new_session=True)
            try:
                if timeout:
                    self.assertTrue(self.second_request.wait(35), 'CLI never reached second request')
                    if before_stop is not None:
                        before_stop(root, self)
                    if not timeout_command:
                        os.killpg(process.pid, signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=40)
                if timeout_command:
                    self.assertEqual(process.returncode, 124, stdout[-2000:]+stderr[-2000:])
                if not timeout:
                    self.assertEqual(process.returncode, 0, stdout[-2000:]+stderr[-2000:])
                self.assertFalse(self.errors, self.errors)
                self.assertEqual(len(self.requests), 2, stderr[-2000:])
                for path, headers, body in self.requests:
                    self.assertEqual(path, '/v1/chat/completions')
                    lower = {k.lower():v for k,v in headers.items()}
                    self.assertEqual(lower.get('authorization'), None if profile == 'vllm' else 'Bearer synthetic-provider-key')
                    xai_headers = [k for k in lower if k.startswith(('x-grok', 'x-xai'))]
                    if profile != 'xai': self.assertEqual(xai_headers, [])
                    else: self.assertTrue(xai_headers)
                    self.assertTrue(body['stream'])
                    self.assertTrue(body['stream_options']['include_usage'])
                    self.assertEqual(body['model'], 'synthetic-model')
                    self.assertNotIn('reasoning_effort', body)
                replies = [m for m in self.requests[1][2]['messages'] if m.get('role') == 'tool']
                self.assertTrue(any('synthetic-tool-ok' in json.dumps(m) for m in replies), replies)
                files = list((root/'grok').rglob('usage.json'))
                self.assertEqual(len(files), 1)
                ledger = json.loads(files[0].read_text())
                terminal = None
                if not timeout:
                    if output == 'json': terminal = json.loads(stdout)
                    else:
                        terminal = next(row for row in map(json.loads, stdout.splitlines()) if row.get('type') == 'result')
                if inspect is not None:
                    inspect(root, ledger, terminal, self)
                return ledger, terminal
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.communicate(timeout=5)
                self.release.set()
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    def test_openrouter_full_cli_both_json_surfaces(self):
        for output in ('json', 'streaming-messages-json'):
            with self.subTest(output=output):
                ledger, result = self.run_case(output=output)
                self.assertEqual(ledger['totals']['reasoning_tokens'], 4)
                self.assertEqual(ledger['totals']['cache_creation_tokens'], 7)
                self.assertEqual(ledger['totals']['cost_by_source'], {'openrouter_usage_cost': .03})
                self.assertEqual(result['usage']['input_tokens'], 113)
                self.assertEqual(result['usage']['reasoning_tokens'], 4)
                self.assertEqual(result['total_cost_usd'], .03)
                self.assertIsNone(result['total_cost_usd_ticks'])
                self.assertEqual(result['cost_sources'], ['openrouter_usage_cost'])
                self.assertFalse(result['usage_is_incomplete'])
                self.assertFalse(result['cost_is_partial'])

    def test_reported_zero_and_no_provider_amount(self):
        _, free = self.run_case(money=0)
        self.assertEqual(free['total_cost_usd'], 0)
        ledger, unknown = self.run_case(profile='vllm')
        self.assertIsNone(unknown['total_cost_usd'])
        self.assertEqual(unknown['cost_sources'], [])
        self.assertEqual(ledger['totals']['cost_missing_calls'], 2)

    def test_xai_exact_ticks_remain_compatible(self):
        _, result = self.run_case(profile='xai')
        self.assertEqual(result['total_cost_usd_ticks'], 300_000_000)
        self.assertEqual(result['total_cost_usd'], .03)
        self.assertEqual(result['cost_sources'], ['xai_usage_ticks'])

    def test_partial_and_killed_call_keep_durable_known_amount(self):
        ledger, result = self.run_case(missing_second=True)
        self.assertEqual(ledger['totals']['cost_by_source'], {'openrouter_usage_cost': .01})
        self.assertEqual(ledger['totals']['cost_missing_calls'], 1)
        self.assertIsNone(result['total_cost_usd'])
        self.assertTrue(result['cost_is_partial'])
        ledger, result = self.run_case(timeout=True)
        self.assertIsNone(result)
        self.assertEqual(ledger['totals']['model_calls'], 1)
        self.assertEqual(ledger['totals']['cost_by_source'], {'openrouter_usage_cost': .01})


if __name__ == '__main__':
    unittest.main()
