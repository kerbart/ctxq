package sample

import (
	"fmt"
	alias "example.org/project/helper"
)

type Service[T any] struct {
	Name string
}

// Run formats one value.
func (s *Service[T]) Run(
	value string,
) string {
	raw := `{"brace": "//"}`
	alias.Touch(raw)
	return fmt.Sprintf("%s", value)
}

func Build() *Service[string] {
	return NewService[string]()
}
