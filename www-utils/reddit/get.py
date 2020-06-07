#!/usr/bin/env python3

import sys

if len(sys.argv) < 2:
    exit(f"usage: {sys.argv[0]} url [urls ...]")

import os
import re
import gzip
import json
import http.client
from urllib.parse import urlparse

# NOTE: not going to implement async requests to keep it simple and reddit rate limit
# NOTE: redd.it shortlinks aren't supported

# this link validator also isn't entirely correct but it'll do
valid_link = re.compile(r"^https:\/\/(www\.|old\.)?reddit\.com\/r\/[a-zA-Z0-9-_]+\/comments\/.+")

urls = sys.argv[1:]

def validate(url):
    is_valid = valid_link.match(url)
    if not is_valid:
        print(f"ignoring {url} - not a valid reddit comments link", file=sys.stderr)
    return is_valid

valid_urls = list(map(urlparse, filter(validate, urls)))
conns = {
    netloc: http.client.HTTPSConnection(netloc)
    for netloc in set((url.netloc for url in valid_urls))
}

get_post_data = lambda raw_post: {
    'title': raw_post['title'],
    'subreddit': raw_post['subreddit'],
    'score': raw_post['score'],
    'text': raw_post['selftext'],
    'url': raw_post['url'],
}

get_reply_data = lambda raw_reply: {
    'score': raw_reply['data'].get('score', 'n/a'),
    'text': raw_reply['data'].get('body', 'n/a'),
    'replies': raw_reply['data'].get('replies', 'n/a'),
}

f_namemax = os.statvfs(".").f_namemax
def fmt_save_fn(data):
    name = data['post']['title'][:f_namemax - len(".json.gz")]
    name = name.replace('/', '_')  # naively sanitize file name
    return f"{name}.json.gz"


for u in valid_urls:
    conn = conns[u.netloc]
    conn.request("GET", f"{u.path}.json")  # append .json here; urlparse will strip it from path
    r = conn.getresponse()
    if r.status != 200:
        exit(f"fatal: {u.geturl()} returned code {r.status}")

    try:
        raw_data = json.loads(r.read())
    except json.decoder.JSONDecodeError:
        exit("fatal: response content wasn't json")

    raw_post = raw_data[0]['data']['children'][0]['data']

    replies = [
        get_reply_data(raw_reply)
        for raw_reply in raw_data[1]['data']['children']
    ]

    data = {
        'post': get_post_data(raw_post),
        'replies': replies,
    }

    # populate reply trees for top level replies
    for root in data['replies']:
        q = [root]
        while q:
            cur = q.pop()
            if not isinstance(cur.get('replies', None), dict):  # XXX: sometimes replies will be a string
                continue
            replies = [
                get_reply_data(raw_reply)
                for raw_reply in cur['replies']['data']['children']
            ]
            # rewrite replies list with only the data we're interested in
            cur['replies'] = replies
            q.extend(cur['replies'])

    os.makedirs(data['post']['subreddit'], exist_ok=True)
    with gzip.open(os.path.join(data['post']['subreddit'], fmt_save_fn(data)), 'wt', encoding='utf-8') as f:
        json.dump(data, f, separators=(',', ':'))
