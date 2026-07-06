import { coverBadgeText, coverStatusExplainer, coverStatusLabel } from './cover-preview.model';
import type { ParagraphProvenance } from '../../models';

describe('coverStatusLabel', () => {
  it('names each status in plain prose terms', () => {
    expect(coverStatusLabel('grounded')).toBe('Traced to your evidence');
    expect(coverStatusLabel('unrecorded')).toBe('Needs a look');
    expect(coverStatusLabel('exempt')).toBe('Connecting language');
  });
});

describe('coverStatusExplainer', () => {
  const base = (status: ParagraphProvenance['status']): ParagraphProvenance => ({
    text: 'x',
    status,
    unbacked_tokens: [],
    unbacked_digits: [],
  });

  it('explains a grounded paragraph', () => {
    expect(coverStatusExplainer(base('grounded'))).toContain('all trace to your resume');
  });

  it('explains an exempt paragraph as connective language', () => {
    expect(coverStatusExplainer(base('exempt'))).toContain('no specific claim to check');
  });

  it('lists the specific unbacked tokens and digits for an unrecorded paragraph', () => {
    const p: ParagraphProvenance = {
      text: 'I used Rust for 5 years.',
      status: 'unrecorded',
      unbacked_tokens: ['rust'],
      unbacked_digits: ['5'],
    };
    const text = coverStatusExplainer(p);
    expect(text).toContain('not found in your resume');
    expect(text).toContain('rust');
    expect(text).toContain('5');
  });

  it('omits the list when an unrecorded paragraph names nothing specific', () => {
    const text = coverStatusExplainer(base('unrecorded'));
    expect(text.endsWith('answers.')).toBe(true);
  });
});

describe('coverBadgeText', () => {
  it('reads clean when nothing is flagged', () => {
    expect(coverBadgeText(0, 4)).toBe('Every paragraph traces to your evidence');
  });

  it('has no paragraphs to check when the letter is empty', () => {
    expect(coverBadgeText(0, 0)).toBe('No paragraphs to check yet');
  });

  it('counts flagged paragraphs with agreeing grammar', () => {
    expect(coverBadgeText(3, 5)).toBe('3 of 5 paragraphs need a look');
    expect(coverBadgeText(1, 5)).toBe('1 of 5 paragraphs needs a look');
    expect(coverBadgeText(1, 1)).toBe('1 of 1 paragraph needs a look');
  });
});
