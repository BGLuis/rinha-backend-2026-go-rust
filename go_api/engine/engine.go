package engine

//go:noescape
func SearchVectorFast(q *float32, scratch *byte) int32

//go:noescape
func Pause()
