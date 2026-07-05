import {
  matchSlashCommand,
  parseReply,
  resumeToText,
  slashSuggestions,
  stripMarkers,
} from './artifacts';
import type { ReplySegment } from './artifacts';

/** The artifact kinds a segment list carries, in order (prose runs ignored). */
function cardKinds(segments: ReplySegment[]): string[] {
  return segments.filter((s) => 'artifact' in s).map((s) => (s as { artifact: string }).artifact);
}

/** The prose runs of a segment list, in order. */
function proseRuns(segments: ReplySegment[]): string[] {
  return segments.filter((s) => 'text' in s).map((s) => (s as { text: string }).text);
}

/** Every segment's text joined, to assert no raw sentinel leaked as prose. */
function allProse(segments: ReplySegment[]): string {
  return proseRuns(segments).join(' ');
}

describe('parseReply', () => {
  it('renders a card for a valid marker in the right position', () => {
    const segs = parseReply('Here is the posting.\n⟦artifact:job_description⟧');
    expect(cardKinds(segs)).toEqual(['job_description']);
    expect(proseRuns(segs)).toEqual(['Here is the posting.']);
    expect(allProse(segs)).not.toContain('⟦');
  });

  it('renders a card for each of the three known kinds', () => {
    expect(cardKinds(parseReply('⟦artifact:job_description⟧'))).toEqual(['job_description']);
    expect(cardKinds(parseReply('⟦artifact:resume⟧'))).toEqual(['resume']);
    expect(cardKinds(parseReply('⟦artifact:cover_letter⟧'))).toEqual(['cover_letter']);
  });

  it('drops an unknown kind and shows no card and no sentinel', () => {
    const segs = parseReply('I cannot share that. ⟦artifact:salary⟧');
    expect(cardKinds(segs)).toEqual([]);
    expect(allProse(segs)).toBe('I cannot share that.');
    expect(allProse(segs)).not.toContain('⟦');
    expect(allProse(segs)).not.toContain('salary');
  });

  it('drops an empty marker', () => {
    const segs = parseReply('Sure. ⟦artifact:⟧ done');
    expect(cardKinds(segs)).toEqual([]);
    expect(allProse(segs)).not.toContain('⟦');
  });

  it('renders multiple markers in order, prose preserved between them', () => {
    const segs = parseReply('Both: ⟦artifact:resume⟧ and ⟦artifact:cover_letter⟧ here.');
    expect(cardKinds(segs)).toEqual(['resume', 'cover_letter']);
    expect(proseRuns(segs)).toEqual(['Both:', 'and', 'here.']);
  });

  it('strips an unclosed trailing marker fragment (never a raw sentinel)', () => {
    // The model stopped mid-token: the fragment has no closing bracket, so
    // MARKER_GLOBAL never matches it. It must still be dropped, not shown.
    const segs = parseReply('You can download it here: ⟦artifact:resume');
    expect(cardKinds(segs)).toEqual([]);
    expect(allProse(segs)).toBe('You can download it here:');
    expect(allProse(segs)).not.toContain('⟦');
    expect(allProse(segs)).not.toContain('artifact');
  });

  it('drops a lone trailing open bracket', () => {
    const segs = parseReply('almost there ⟦');
    expect(allProse(segs)).toBe('almost there');
    expect(allProse(segs)).not.toContain('⟦');
  });

  it('leaves a marker-free reply as a single prose run', () => {
    const segs = parseReply('Just plain advice, no attachments.');
    expect(cardKinds(segs)).toEqual([]);
    expect(proseRuns(segs)).toEqual(['Just plain advice, no attachments.']);
  });
});

describe('stripMarkers', () => {
  it('removes a complete marker from mid-stream text', () => {
    expect(stripMarkers('here it is ⟦artifact:resume⟧ enjoy')).not.toContain('⟦');
  });

  it('removes a half-arrived trailing marker so it never flashes', () => {
    // Each of these is a snapshot of the same marker still streaming in.
    expect(stripMarkers('showing it ⟦')).toBe('showing it ');
    expect(stripMarkers('showing it ⟦artif')).toBe('showing it ');
    expect(stripMarkers('showing it ⟦artifact:res')).toBe('showing it ');
    expect(stripMarkers('showing it ⟦artifact:resume')).toBe('showing it ');
  });

  it('does not eat a lone open bracket sitting in legitimate prose', () => {
    // Not at the end and not part of a marker: it is ordinary text.
    const out = stripMarkers('use the ⟦ symbol in your notes');
    expect(out).toBe('use the ⟦ symbol in your notes');
  });
});

describe('matchSlashCommand', () => {
  it('matches a leading slash command by name', () => {
    expect(matchSlashCommand('/jd')?.name).toBe('jd');
    expect(matchSlashCommand('/resume')?.name).toBe('resume');
    expect(matchSlashCommand('/cover')?.name).toBe('cover');
  });

  it('matches an alias', () => {
    expect(matchSlashCommand('/posting')?.name).toBe('jd');
    expect(matchSlashCommand('/letter')?.name).toBe('cover');
  });

  it('is case-insensitive and tolerates surrounding whitespace', () => {
    expect(matchSlashCommand('  /JD  ')?.name).toBe('jd');
  });

  it('does not match a slash that is not at the start', () => {
    expect(matchSlashCommand('please show /jd now')).toBeNull();
    expect(matchSlashCommand('what about a/b testing')).toBeNull();
  });

  it('returns null for an unknown command', () => {
    expect(matchSlashCommand('/foo')).toBeNull();
    expect(matchSlashCommand('/skills')).toBeNull();
  });

  it('returns null for a non-slash message', () => {
    expect(matchSlashCommand('show me the posting')).toBeNull();
  });
});

describe('slashSuggestions', () => {
  it('lists every command for a bare slash', () => {
    expect(slashSuggestions('/').map((c) => c.name)).toEqual(['jd', 'resume', 'cover']);
  });

  it('filters by the partial word, name or alias', () => {
    expect(slashSuggestions('/re').map((c) => c.name)).toEqual(['resume']);
    expect(slashSuggestions('/co').map((c) => c.name)).toEqual(['cover']);
    expect(slashSuggestions('/post').map((c) => c.name)).toEqual(['jd']);
  });

  it('stops suggesting once the command word is complete (a space follows)', () => {
    expect(slashSuggestions('/jd ')).toEqual([]);
  });

  it('offers nothing for a non-slash draft', () => {
    expect(slashSuggestions('hello')).toEqual([]);
  });
});

describe('resumeToText', () => {
  it('projects stored fields with no em dash in the heading', () => {
    const text = resumeToText({
      contact: { full_name: 'Sam Rivera' },
      target_title: 'Engineering Manager',
      summary: 'Leader.',
      roles: [{ title: 'Director', company: 'Acme', bullets: [{ text: 'Ran on-call.' }] }],
      skills_section: { skills: ['Leadership', 'TypeScript'] },
    });
    expect(text).toContain('Sam Rivera');
    expect(text).toContain('Director · Acme');
    expect(text).toContain('  - Ran on-call.');
    expect(text).toContain('Leadership, TypeScript');
    expect(text).not.toContain('—');
    expect(text).not.toContain('–');
  });

  it('tolerates a sparse draft', () => {
    expect(resumeToText({})).toBe('');
    expect(resumeToText(null)).toBe('');
  });
});
