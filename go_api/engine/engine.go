package engine

//go:noescape
func SearchVectorFast(q *float32, scratch *byte) float32
