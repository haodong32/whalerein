package store

type syntheticRow struct {
	fill func(...any)
	err  error
}

func (r syntheticRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	r.fill(dest...)
	return nil
}
