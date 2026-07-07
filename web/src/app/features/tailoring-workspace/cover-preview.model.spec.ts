import {
  coverBadgeText,
  coverSaveMessage,
  coverStatusExplainer,
  coverStatusLabel,
  coverUnrecordedFlag,
} from './cover-preview.model';
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
    expect(text.endsWith('the posting.')).toBe(true);
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

describe('coverUnrecordedFlag', () => {
  it('is null when nothing is unrecorded', () => {
    expect(coverUnrecordedFlag(0)).toBeNull();
    expect(coverUnrecordedFlag(-1)).toBeNull();
  });

  it('warns with agreeing grammar when paragraphs are unrecorded', () => {
    expect(coverUnrecordedFlag(1)).toBe(
      "1 paragraph isn't traced to your evidence yet. Saving keeps them as written.",
    );
    expect(coverUnrecordedFlag(2)).toBe(
      "2 paragraphs aren't traced to your evidence yet. Saving keeps them as written.",
    );
  });
});

describe('coverSaveMessage', () => {
  it('reports dropped paragraphs first, distinct from the unrecorded flag', () => {
    expect(coverSaveMessage(1, 0)).toBe('Saved. 1 paragraph with an unverified number was dropped.');
    expect(coverSaveMessage(2, 3)).toBe(
      'Saved. 2 paragraphs with an unverified number were dropped.',
    );
  });

  it('flags remaining unrecorded paragraphs when none were dropped', () => {
    expect(coverSaveMessage(0, 1)).toBe("Saved, but 1 paragraph still isn't traced to your evidence.");
    expect(coverSaveMessage(0, 2)).toBe("Saved, but 2 paragraphs still aren't traced to your evidence.");
  });

  it('is a plain confirmation when the save is clean', () => {
    expect(coverSaveMessage(0, 0)).toBe('Saved your cover letter edits.');
  });
});
