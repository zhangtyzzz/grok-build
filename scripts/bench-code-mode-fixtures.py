#!/usr/bin/env python3
"""Generate only inputs; print prompts and expected answers for the operator."""
import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('task', choices=['dependent', 'parallel', 'tiny'])
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=False)

    def write(name, value):
        (args.directory / name).write_text(json.dumps(value, indent=2))

    if args.task == 'dependent':
        # index/release use compact JSON, as in the measured fixtures.
        (args.directory / 'index.json').write_text(json.dumps({'active_release': 'release-17.json'}))
        (args.directory / 'release-17.json').write_text(json.dumps({
            'selected_ids': [17, 93, 208, 349],
            'shards': ['part-%d.json' % i for i in range(4)],
        }))
        for shard in range(4):
            write('part-%d.json' % shard, [
                {'id': i, 'amount': (i * 37 + 19) % 997,
                 'note': 'fixture-' + str(i) + '-abcdefghijklmno'}
                for i in range(shard * 100, (shard + 1) * 100)
            ])
        prompt = 'Follow index.json to the active release. Sum amount for exactly its selected_ids across the listed shards. Return only JSON {"sum": integer, "count": integer}.'
        expected = {'sum': 2821, 'count': 4}
    elif args.task == 'parallel':
        expected = {}
        for shard in range(4):
            rows = [
                {'service': 'svc-' + str(shard),
                 'status': 'error' if (i + shard) % 7 == 0 else 'ok',
                 'latency': (i * 11) % 300}
                for i in range(90)
            ]
            write('service-%d.json' % shard, rows)
            expected['svc-' + str(shard)] = sum(r['status'] == 'error' for r in rows)
        prompt = 'Count the records with status exactly error separately in service-0.json through service-3.json. Return only a JSON object mapping svc-0 through svc-3 to their error counts.'
    else:
        (args.directory / 'settings.json').write_text(json.dumps({'port': 8317, 'retries': 3, 'region': 'eu-test'}))
        prompt = 'Read settings.json and return only JSON {"port": integer}.'
        expected = {'port': 8317}
    print(json.dumps({'prompt': prompt, 'expected': expected}, indent=2))


if __name__ == '__main__':
    main()
