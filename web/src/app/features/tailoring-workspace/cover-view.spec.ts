import { coverExists } from './cover-view';

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
