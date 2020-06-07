#!/usr/bin/env python3

import sys

if len(sys.argv) < 2:
    exit(f"usage: {sys.argv[0]} id [ids ...]")

import os
import gzip
import json
import http.client

# NOTE: not going to implement async requests for now to keep it simple

ids = sys.argv[1:]
for _id in ids:
    if not _id.isdigit() or int(_id) < 1:
        exit("ids must be integers >= 1.")

conn = http.client.HTTPSConnection("hacker-news.firebaseio.com")

def fetch(_id):
    conn.request("GET", f"/v0/item/{_id}.json")  # append .json here; urlparse will strip it from path
    r = conn.getresponse()
    if r.status != 200:
        exit(f"fatal: fetching {_id} returned code {r.status}")
    try:
        return json.loads(r.read())
    except json.decoder.JSONDecodeError:
        exit("fatal: response content wasn't json")


# key filter for story (story, ask HN) posts
# text field is unique to ask HN
story_keys = (
    "by",
    "id",
    "kids",
    "score",
    "text",
    "time",
    "title",
    "url",
)

filter_story_data = lambda raw_data: {
    k: raw_data.get(k, "")
    for k in story_keys
}

comment_keys = tuple(set(story_keys) - {
    "score",
    "url",
})

filter_comment_data = lambda raw_data: {
    k: raw_data.get(k, "")
    for k in comment_keys
}

def get_comments(comment_ids):
    comments_raw_data = [
        fetch(comment_id)
        for comment_id in story["kids"]
    ]
    comments = [
        filter_comment_data(raw_data)
        for raw_data in comments_raw_data
    ]
    return comments

f_namemax = os.statvfs(".").f_namemax
def fmt_save_fn(story):
    name = f"{story['title']}-{_id}"[:f_namemax - len(".json.gz")]
    name = name.replace('/', '_')  # naively sanitize file name
    return f"{name}.json.gz"


for _id in ids:
    raw_data = fetch(_id)
    if raw_data["type"] != "story":
        print(f"id {_id} doesn't seem to be a story post - ignoring", file=sys.stderr)

    story = filter_story_data(raw_data)
    story["kids"] = get_comments(story["kids"])

    # populate comment trees for top level comment
    for root in story["kids"]:
        q = [root]
        while q:
            cur = q.pop()
            cur["kids"] = get_comments(story["kids"])
            q.extend(cur["kids"])

    with gzip.open(fmt_save_fn(story), 'wt', encoding='utf-8') as f:
        json.dump(story, f, separators=(',', ':'))
