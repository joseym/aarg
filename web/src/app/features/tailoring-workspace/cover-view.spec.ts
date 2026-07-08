import { HttpErrorResponse } from '@angular/common/http';

import { coverExists, coverRecheckErrorMessage, isEmptyBrief } from './cover-view';
import type { CoverBrief } from '../../models';

describe('coverExists', () => {
  it('detects a rendered cover letter in the build pdfs list', () => {
    expect(coverExists(['resume.ats.pdf', 'resume.human.pdf', 'cover_letter.pdf'])).toBe(true);
  });

  it('is false when no cover has been rendered', () => {
    expect(coverExists(['resume.ats.pdf'])).toBe(false);
    expect(coverExists([])).toBe(false);
    expect(coverExists(undefined)).toBe(false);
  });
});

describe('isEmptyBrief', () => {
  const blank: CoverBrief = {
    angle: null,
    emphasis: [],
    tone: null,
    motivation: null,
    constraints: [],
  };

  it('is true for null/undefined (no interview ran at all)', () => {
    expect(isEmptyBrief(null)).toBe(true);
    expect(isEmptyBrief(undefined)).toBe(true);
  });

  it('is true for a brief with every field blank (a cancel with nothing answered)', () => {
    expect(isEmptyBrief(blank)).toBe(true);
    // Whitespace-only scalars count as blank too.
    expect(isEmptyBrief({ ...blank, angle: '   ' })).toBe(true);
  });

  it('is false once any single field carries an answer', () => {
    expect(isEmptyBrief({ ...blank, angle: 'lead with reliability' })).toBe(false);
    expect(isEmptyBrief({ ...blank, emphasis: ['incident response'] })).toBe(false);
    expect(isEmptyBrief({ ...blank, tone: 'direct' })).toBe(false);
    expect(isEmptyBrief({ ...blank, motivation: 'used their product for years' })).toBe(false);
    expect(isEmptyBrief({ ...blank, constraints: ['skip my current employer'] })).toBe(false);
  });
});

describe('coverRecheckErrorMessage', () => {
  it('names the check as what failed, not the letter or a paragraph', () => {
    const msg = coverRecheckErrorMessage(new Error('network error'));
    expect(msg).toContain("Couldn’t check this letter’s provenance right now");
    expect(msg).not.toContain('unrecorded');
  });

  it('carries a plain Error message through', () => {
    expect(coverRecheckErrorMessage(new Error('timed out'))).toContain('timed out');
  });

  it('carries a bare string reason through', () => {
    expect(coverRecheckErrorMessage('bad key')).toContain('bad key');
  });

  it('unwraps an HttpErrorResponse envelope message', () => {
    const err = new HttpErrorResponse({
      error: { error: { message: 'the model proxy is unreachable' } },
      status: 502,
    });
    expect(coverRecheckErrorMessage(err)).toContain('the model proxy is unreachable');
  });

  it('falls back to a generic reason for an unrecognized error shape', () => {
    expect(coverRecheckErrorMessage({})).toContain('the request failed');
  });
});
