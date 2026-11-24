#!/usr/bin/env python3
"""Generates the Bedrock data of `vanillite-bedrock` (crates/vanillite-bedrock/data/).

Inputs (MIT, GeyserMC): a checkout of https://github.com/GeyserMC/mappings (Java <-> Bedrock
block and biome mappings) and the `core/src/main/resources/bedrock` directory of
https://github.com/GeyserMC/Geyser (Bedrock block palette, items, biomes, entity identifiers).
Plus Vanillite's own `data/vanilla/blocks.json` for the Java block state order.

Usage: generate.py <mappings dir> <geyser bedrock resources dir> <palette version, e.g. 26_60>

Outputs (all little-endian, Bedrock network NBT where NBT is involved):
- blocks.bin: u32 count, u32 runtime id of still water, then one u16 per Java block state id:
  the Bedrock runtime id, bit 15 set when the state is waterlogged (water in the second layer).
- biomes.txt: `<java biome name> <bedrock biome id>` per line.
- block_properties.bin: the `block properties` list of `start_game` (count + name + NBT).
- item_registry.bin: the body of `item_registry` (count + entries).
- biome_definitions.bin: the body of `biome_definition_list`.
- entity_identifiers.bin: the body of `available_actor_identifiers` (network NBT).
- items.bin: u32 count, then per Java item id: i16 Bedrock item id, u16 Bedrock metadata (0/0 for
  Java items without a Bedrock counterpart).
- creative.bin: the body of `creative_content` (groups and items, Bedrock's creative order).
- creative_java.bin: u32 count, then per creative item network id (1-based, in order): u16 Java
  item id.
- sounds.txt: `<java sound event> <bedrock sound name>` per line.
"""

import base64

import gzip
import json
import os
import struct
import sys

# ---- Java (big-endian) NBT reader -------------------------------------------------------------


class BeReader:
    def __init__(self, data):
        self.d = data
        self.p = 0

    def take(self, n):
        v = self.d[self.p:self.p + n]
        self.p += n
        return v

    def u8(self):
        return self.take(1)[0]

    def i8(self):
        return struct.unpack('>b', self.take(1))[0]

    def i16(self):
        return struct.unpack('>h', self.take(2))[0]

    def i32(self):
        return struct.unpack('>i', self.take(4))[0]

    def i64(self):
        return struct.unpack('>q', self.take(8))[0]

    def string(self):
        n = struct.unpack('>H', self.take(2))[0]
        return self.take(n).decode('utf-8')

    def payload(self, t):
        if t == 1:
            return ('b', self.i8())
        if t == 2:
            return ('s', self.i16())
        if t == 3:
            return ('i', self.i32())
        if t == 4:
            return ('l', self.i64())
        if t == 5:
            return ('f', struct.unpack('>f', self.take(4))[0])
        if t == 6:
            return ('d', struct.unpack('>d', self.take(8))[0])
        if t == 7:
            n = self.i32()
            return ('ba', self.take(n))
        if t == 8:
            return ('str', self.string())
        if t == 9:
            et = self.u8()
            n = self.i32()
            return ('list', et, [self.payload(et) for _ in range(n)])
        if t == 10:
            entries = []
            while True:
                et = self.u8()
                if et == 0:
                    return ('c', entries)
                name = self.string()
                entries.append((name, self.payload(et)))
        if t == 11:
            n = self.i32()
            return ('ia', [self.i32() for _ in range(n)])
        if t == 12:
            n = self.i32()
            return ('la', [self.i64() for _ in range(n)])
        raise ValueError(f'tag type {t}')

    def root(self):
        t = self.u8()
        self.string()
        return self.payload(t)


class LeReader(BeReader):
    """Little-endian (Bedrock disk) NBT."""

    def i16(self):
        return struct.unpack('<h', self.take(2))[0]

    def i32(self):
        return struct.unpack('<i', self.take(4))[0]

    def i64(self):
        return struct.unpack('<q', self.take(8))[0]

    def string(self):
        n = struct.unpack('<H', self.take(2))[0]
        return self.take(n).decode('utf-8')

    def payload(self, t):
        if t == 5:
            return ('f', struct.unpack('<f', self.take(4))[0])
        if t == 6:
            return ('d', struct.unpack('<d', self.take(8))[0])
        return super().payload(t)


def read_gz_nbt(path):
    return BeReader(gzip.decompress(open(path, 'rb').read())).root()


def compound_get(c, key, default=None):
    for k, v in c[1]:
        if k == key:
            return v
    return default


# ---- Bedrock network NBT writer ----------------------------------------------------------------

TYPE_IDS = {'b': 1, 's': 2, 'i': 3, 'l': 4, 'f': 5, 'd': 6, 'ba': 7, 'str': 8, 'list': 9, 'c': 10,
            'ia': 11, 'la': 12}


def varuint(v):
    out = bytearray()
    while True:
        b = v & 0x7f
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def zigzag32(v):
    return varuint(((v << 1) ^ (v >> 31)) & 0xffffffff)


def zigzag64(v):
    return varuint(((v << 1) ^ (v >> 63)) & 0xffffffffffffffff)


def net_string(s):
    b = s.encode('utf-8')
    return varuint(len(b)) + b


def net_payload(tag):
    kind = tag[0]
    if kind == 'b':
        return struct.pack('<b', tag[1])
    if kind == 's':
        return struct.pack('<h', tag[1])
    if kind == 'i':
        return zigzag32(tag[1])
    if kind == 'l':
        return zigzag64(tag[1])
    if kind == 'f':
        return struct.pack('<f', tag[1])
    if kind == 'd':
        return struct.pack('<d', tag[1])
    if kind == 'ba':
        return zigzag32(len(tag[1])) + tag[1]
    if kind == 'str':
        return net_string(tag[1])
    if kind == 'list':
        return bytes([tag[1]]) + zigzag32(len(tag[2])) + b''.join(net_payload(e) for e in tag[2])
    if kind == 'c':
        out = bytearray()
        for name, v in tag[1]:
            out.append(TYPE_IDS[v[0]])
            out += net_string(name)
            out += net_payload(v)
        out.append(0)
        return bytes(out)
    if kind == 'ia':
        return zigzag32(len(tag[1])) + b''.join(zigzag32(v) for v in tag[1])
    if kind == 'la':
        return zigzag32(len(tag[1])) + b''.join(zigzag64(v) for v in tag[1])
    raise ValueError(kind)


def net_root(compound):
    return b'\x0a' + net_string('') + net_payload(compound)


def canonical(tag):
    """A hashable form of an NBT value for palette lookups (state order does not matter)."""
    kind = tag[0]
    if kind == 'c':
        return ('c', tuple(sorted((k, canonical(v)) for k, v in tag[1])))
    if kind == 'list':
        return ('list', tuple(canonical(e) for e in tag[2]))
    if kind in ('ba',):
        return (kind, bytes(tag[1]))
    if kind in ('ia', 'la'):
        return (kind, tuple(tag[1]))
    # Bedrock block states use byte, int and string; normalise numbers to their value.
    return ('n' if kind in ('b', 's', 'i', 'l') else kind, tag[1])


# ---- Generation ----------------------------------------------------------------------------------

def main():
    mappings, resources, version = sys.argv[1], sys.argv[2], sys.argv[3]
    root = os.path.normpath(os.path.join(os.path.dirname(__file__), '..', '..'))
    out_dir = os.path.join(root, 'crates', 'vanillite-bedrock', 'data')
    os.makedirs(out_dir, exist_ok=True)

    # Java block state id -> Java block name and whether it holds water.
    always_wet = {'minecraft:kelp', 'minecraft:kelp_plant', 'minecraft:seagrass',
                  'minecraft:tall_seagrass', 'minecraft:bubble_column'}
    java_names = []
    wet = []
    for block in json.load(open(os.path.join(root, 'data', 'vanilla', 'blocks.json'))):
        assert block['first_state'] == len(java_names), block['name']
        java_names += [block['name']] * block['state_count']
        props = block['properties']
        for i in range(block['state_count']):
            w = block['name'] in always_wet
            stride = 1
            for prop in reversed(props):
                value = prop['values'][(i // stride) % len(prop['values'])]
                stride *= len(prop['values'])
                if prop['name'] == 'waterlogged' and value == 'true':
                    w = True
            wet.append(w)

    # Bedrock palette: runtime id = index (the order the client uses).
    palette = compound_get(read_gz_nbt(os.path.join(resources, f'block_palette.{version}.nbt')), 'blocks')
    runtime = {}
    air = None
    for i, entry in enumerate(palette[2]):
        name = compound_get(entry, 'name')[1]
        states = compound_get(entry, 'states')
        runtime[(name, canonical(states))] = i
        if name == 'minecraft:air':
            air = i
    assert air is not None
    water = runtime[('minecraft:water', canonical(('c', [('liquid_depth', ('i', 0))])))]

    # Java state -> Bedrock name + states (Geyser's mapping, index = Java state id).
    mapped = compound_get(read_gz_nbt(os.path.join(mappings, 'blocks.nbt')), 'bedrock_mappings')[2]
    assert len(mapped) == len(java_names), (len(mapped), len(java_names))
    table = []
    missing = 0
    for java_id, entry in enumerate(mapped):
        ident = compound_get(entry, 'bedrock_identifier')
        name = 'minecraft:' + (ident[1] if ident else java_names[java_id].split(':', 1)[1])
        states = compound_get(entry, 'state', ('c', []))
        rid = runtime.get((name, canonical(states)))
        if rid is None:
            missing += 1
            rid = air
        table.append(rid)
    assert max(table) < 1 << 15
    table = [rid | (0x8000 if w else 0) for rid, w in zip(table, wet)]
    with open(os.path.join(out_dir, 'blocks.bin'), 'wb') as f:
        f.write(struct.pack('<II', len(table), water))
        f.write(struct.pack(f'<{len(table)}H', *table))
    print(f'blocks: {len(table)} Java states, {len(runtime)} Bedrock states, {missing} unmapped')

    # Biomes.
    biomes = json.load(open(os.path.join(mappings, 'biomes.json')))
    with open(os.path.join(out_dir, 'biomes.txt'), 'w') as f:
        for name, v in sorted(biomes.items()):
            f.write(f"{name} {v['bedrock_id']}\n")

    # Block properties of start_game (data-driven vanilla blocks).
    ddb = read_gz_nbt(os.path.join(resources, f'data_driven_blocks.{version}.nbt'))
    out = bytearray(varuint(len(ddb[1])))
    for name, props in ddb[1]:
        out += net_string(name) + net_root(props)
    open(os.path.join(out_dir, 'block_properties.bin'), 'wb').write(out)

    # Item registry.
    items = json.load(open(os.path.join(resources, f'runtime_item_states.{version}.json')))
    components = dict(read_gz_nbt(os.path.join(resources, f'item_components.{version}.nbt'))[1])
    out = bytearray(varuint(len(items)))
    for item in items:
        out += net_string(item['name'])
        out += struct.pack('<h', item['id'])
        out += bytes([1 if item['componentBased'] else 0])
        out += zigzag32(item['version'])
        out += net_root(components.get(item['name'], ('c', [])))
    open(os.path.join(out_dir, 'item_registry.bin'), 'wb').write(out)

    # Biome definitions (structured format since 1.21.80; vanilla biomes carry no id: -1).
    defs = json.load(open(os.path.join(resources, 'stripped_biome_definitions.json')))
    strings = []
    index = {}

    def string_index(s):
        if s not in index:
            index[s] = len(strings)
            strings.append(s)
        return index[s]

    out = bytearray(varuint(len(defs)))
    for name, d in defs.items():
        out += struct.pack('<h', string_index(name))
        out += struct.pack('<h', -1 if d.get('id') is None else d['id'])
        out += struct.pack('<5f', d['temperature'], d['downfall'], d.get('foliageSnow', 0.0),
                           d['depth'], d['scale'])
        c = d['mapWaterColor']
        out += struct.pack('<I', (c['a'] << 24) | (c['r'] << 16) | (c['g'] << 8) | c['b'])
        out += bytes([1 if d['rain'] else 0])
        tags = d.get('tags')
        if tags is None:
            out += b'\x00'
        else:
            out += b'\x01' + varuint(len(tags))
            for t in tags:
                out += struct.pack('<h', string_index(t))
        out += b'\x00'  # no chunk generation data
    out += varuint(len(strings))
    for s in strings:
        out += net_string(s)
    open(os.path.join(out_dir, 'biome_definitions.bin'), 'wb').write(out)

    # Items: Java item id -> Bedrock item id and metadata.
    bedrock_item_ids = {item['name']: item['id'] for item in items}
    item_map = json.load(open(os.path.join(mappings, 'items.json')))
    java_items = [i['name'] for i in json.load(open(os.path.join(root, 'data', 'vanilla', 'items.json')))]
    table = []
    reverse = {}
    unmapped = []
    for java_id, name in enumerate(java_items):
        m = item_map.get(name)
        bid = bedrock_item_ids.get(m['bedrock_identifier']) if m else None
        if bid is None:
            if name != 'minecraft:air':
                unmapped.append(name)
            table.append((0, 0))
            continue
        meta = m.get('bedrock_data', 0)
        table.append((bid, meta))
        reverse.setdefault((bid, meta), java_id)
    with open(os.path.join(out_dir, 'items.bin'), 'wb') as f:
        f.write(struct.pack('<I', len(table)))
        for bid, meta in table:
            f.write(struct.pack('<hH', bid, meta))
    print(f'items: {len(table)} Java items, unmapped: {unmapped}')

    # Creative inventory: Bedrock's groups and order, items that have a Java counterpart.
    creative = json.load(open(os.path.join(resources, f'creative_items.{version}.json')))

    def block_rid(entry):
        b64 = entry.get('block_state_b64')
        if not b64:
            return 0
        tag = LeReader(base64.b64decode(b64)).root()
        name = compound_get(tag, 'name')[1]
        return runtime.get((name, canonical(compound_get(tag, 'states', ('c', [])))), 0)

    def item_bytes(entry, count):
        bid = bedrock_item_ids.get(entry['id'], 0)
        out = zigzag32(bid)
        if bid == 0:
            return out + struct.pack('<H', 0) + varuint(0) + zigzag32(0) + varuint(0)
        out += struct.pack('<H', count) + varuint(entry.get('damage', 0))
        out += zigzag32(block_rid(entry))
        user = struct.pack('<h', 0) + struct.pack('<I', 0) + struct.pack('<I', 0)
        return out + varuint(len(user)) + user

    categories = {'construction': 1, 'nature': 2, 'equipment': 3, 'items': 4}
    groups = creative['groups']
    out = bytearray(varuint(len(groups)))
    for g in groups:
        out += bytes([categories[g['category']]]) + net_string(g['name']) + item_bytes(g['icon'], 1)
    entries = []
    for entry in creative['items']:
        if entry.get('nbt_b64'):
            continue
        bid = bedrock_item_ids.get(entry['id'])
        java_id = reverse.get((bid, entry.get('damage', 0)))
        if java_id is None:
            continue
        entries.append((entry, java_id))
    out += varuint(len(entries))
    for net_id, (entry, _) in enumerate(entries, 1):
        out += varuint(net_id) + item_bytes(entry, 1) + varuint(entry['groupId'])
    open(os.path.join(out_dir, 'creative.bin'), 'wb').write(out)
    with open(os.path.join(out_dir, 'creative_java.bin'), 'wb') as f:
        f.write(struct.pack('<I', len(entries)))
        for _, java_id in entries:
            f.write(struct.pack('<H', java_id))
    print(f'creative: {len(entries)} of {len(creative["items"])} items')

    # Sounds: Java sound event -> Bedrock sound name.
    sounds = json.load(open(os.path.join(mappings, 'sounds.json')))
    with open(os.path.join(out_dir, 'sounds.txt'), 'w') as f:
        for name, v in sorted(sounds.items()):
            if v.get('playsound_mapping'):
                f.write(f"minecraft:{name} {v['playsound_mapping']}\n")

    # Entity identifiers: already network NBT.
    data = open(os.path.join(resources, 'entity_identifiers.dat'), 'rb').read()
    open(os.path.join(out_dir, 'entity_identifiers.bin'), 'wb').write(data)


if __name__ == '__main__':
    main()
